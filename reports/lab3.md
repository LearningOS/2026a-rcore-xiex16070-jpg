# 实验三 进程管理与调度（ch5）

## 1. 实验目的

- 在 ch4 的「每个进程一套地址空间」基础上，接通 `spawn` / `wait` / `waitpid`，理解父子进程与僵尸进程的回收；
- 用 **stride 调度**替换简单的轮转调度，让 `set_priority` 的优先级真正体现为 CPU 时间占比；
- 把 ch4 的 `mmap` / `munmap` / `get_time` 移植到「按进程隔离」的地址空间上，处理区间重叠与整块回收；
- 理解 initproc（`ch5b_initproc`）为什么能一次拉起 usertest 里的全部用户程序。

## 2. 实验环境

| 项目 | 内容 |
| --- | --- |
| 代码基线 | `LearningOS/2026a-rcore-xiex16070-jpg` 仓库 `ch5` 分支 |
| 官方检查器 | `rCore-Tutorial-Checker` @ `7d61ec55b58eed6ca7052917846c1b87af34563a` |
| 官方测试集 | `rCore-Tutorial-Test` @ `a0593662ad55d670ba8c27ce1763347cd0dd552f` |
| 工具链 | Rust `nightly-2024-05-02`，目标 `riscv64gc-unknown-none-elf` |
| 运行环境 | `qemu-system-riscv64 -machine virt`，`qemu 7.0.0` 及以上 |

## 3. 实验内容

ch5 的内核里，「任务」变成了「进程 + 独立地址空间」。模板已经提供了 `fork` / `exec` /
`wait` / `waitpid` 的骨架，本实验补的是：`spawn`、`set_priority` 与 stride 调度、
`mmap` / `munmap`、`get_time` 的进程内实现。

### 3.1 sys_spawn：按名字直接拉起进程

`sys_spawn` 与 `fork` + `exec` 的区别是：子进程不复制父进程的地址空间，而是**直接**拿
目标 ELF 新建一套。名字 → ELF 的映射由链接进内核的 `_app_names` / `_num_app` 表提供
（`get_app_data_by_name`）：

```rust
// os/src/syscall/process.rs
pub fn sys_spawn(path: *const u8) -> isize {
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(data) = get_app_data_by_name(path.as_str()) {
        let parent = current_task().unwrap();
        let new_task = Arc::new(TaskControlBlock::new(data));
        let new_pid = new_task.pid.0;
        new_task.inner_exclusive_access().parent = Some(Arc::downgrade(&parent));
        parent.inner_exclusive_access().children.push(new_task.clone());
        add_task(new_task);            // 进入就绪队列
        new_pid
    } else {
        -1
    }
}
```

父进程用 `Arc<TaskControlBlock>` 强引用子进程、子进程用 `Weak` 回指父进程，是为了打破
循环引用：父进程先退出时，子进程不能因此被永久钉在内存里。

### 3.2 stride 调度

每个进程有两个数：`stride`（每被调度一次 `pass` 的增量）与 `pass`（累计值）。

```rust
// os/src/task/task.rs
pub const BIG_STRIDE: usize = 1 << 20;      // 默认优先级对应的 stride
pub const DEFAULT_PRIORITY: usize = 16;
// TaskControlBlock::new / fork:
stride: BIG_STRIDE / DEFAULT_PRIORITY,
pass: 0,
```

调度器每次挑 `pass` 最小的进程；进程离开 CPU 时把 `pass` 加上自己的 `stride`：

```rust
// os/src/task/manager.rs
pub fn add(&mut self, task: Arc<TaskControlBlock>) {
    let mut inner = task.inner_exclusive_access();
    inner.pass += inner.stride;             // 本次时间片已消耗
    drop(inner);
    self.ready_queue.push_back(task);
}

pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
    let (idx, _) = self.ready_queue.iter().enumerate()
        .min_by_key(|(_, task)| task.inner_exclusive_access().pass)?;
    Some(self.ready_queue.remove(idx))
}
```

优先级越高，`stride = BIG_STRIDE / priority` 越小，`pass` 增长越慢，被选中的次数就越多，
CPU 占比自然接近 `priority / Σpriority`。

### 3.3 sys_set_priority 的边界

```rust
pub fn sys_set_priority(prio: isize) -> isize {
    if prio < 2 { return -1; }                       // 优先级下限
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    inner.stride = (BIG_STRIDE / prio as usize).max(1);   // 防止 stride 退化为 0
    prio
}
```

`prio` 是 `isize`，必须先拒绝非正数再转 `usize`；而 `BIG_STRIDE / prio` 对极大的 `prio`
可能得到 0，`stride == 0` 会让该进程的 `pass` 永远不涨、独占 CPU，所以用 `.max(1)` 兜底。

### 3.4 mmap / munmap 的区间管理

ch5 里 `mmap` 的语义比 ch4 更严格：申请区间不能与已有 `MapArea` 重叠，`munmap` 必须
覆盖某个完整的 `MapArea`。

```rust
// os/src/mm/memory_set.rs
pub fn overlaps(&self, start_va: VirtAddr, end_va: VirtAddr) -> bool {
    let (start_vpn, end_vpn) = (start_va.floor(), end_va.ceil());
    self.areas.iter().any(|area| {
        start_vpn < area.vpn_range.get_end() && area.vpn_range.get_start() < end_vpn
    })
}

pub fn remove_area(&mut self, start_va: VirtAddr, end_va: VirtAddr) -> bool {
    let (start_vpn, end_vpn) = (start_va.floor(), end_va.ceil());
    if let Some(idx) = self.areas.iter().position(|area|
        area.vpn_range.get_start() == start_vpn && area.vpn_range.get_end() == end_vpn
    ) {
        let mut area = self.areas.remove(idx);
        area.unmap(&mut self.page_table);            // 逐页清 PTE，帧由 FrameTracker 归还
        true
    } else {
        false
    }
}
```

`sys_mmap` 还要求 `start` 页对齐、`len != 0`、`port` 只允许低 3 位（`R/W/X`）；
`sys_munmap` 要求 `start` 与 `len` 都页对齐，否则返回 -1。

### 3.5 sys_get_time 与页表

ch5 的用户地址空间各自独立，内核不能直接往用户指针写数据，必须走
`translated_byte_buffer(token, ...)` 得到物理可写切片后再拷贝：

```rust
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    let time = get_time_us();
    let time_val = TimeVal { sec: time / 1_000_000, usec: time % 1_000_000 };
    let src = unsafe { core::slice::from_raw_parts(&time_val as *const _ as *const u8,
                                                   core::mem::size_of::<TimeVal>()) };
    let mut copied = 0;
    for buffer in translated_byte_buffer(current_user_token(), ts as *const u8, src.len()) {
        let n = buffer.len().min(src.len() - copied);
        buffer[..n].copy_from_slice(&src[copied..copied + n]);
        copied += n;
    }
    if copied == src.len() { 0 } else { -1 }
}
```

## 4. 测试结果

`python tools\local_ci.py 5 --timeout 120`（本地复刻官方 checker）：

```
[PASS] found <Test spawn0 OK!>
[PASS] found <Test wait OK!>
[PASS] found <Test waitpid OK!>
[PASS] found <Test set_priority OK!>
...
Test passed: 15/15
```

串口日志中 initproc 依次拉起 18 个目标程序（`Usertests: Running ch2b_hello_world` …
`Usertests: Running ch5_setprio`），全部 `exited with code 0`，最后打印
`ch5 Usertests passed!`。`ch5_setprio` 里 12 个 `Test getpid OK! pid = N` 说明
stride 调度确实在同优先级进程之间轮转；`Test set_priority OK!` 说明改优先级后
`pass` 的追赶行为符合预期。

## 5. 遇到的问题

1. **initproc 是复制出来的**：checker 把 `ch5_usertest.elf` 复制成
   `ch5b_initproc.elf`，所以 `spawn` 必须依赖 `_app_names` 而不是按序号取 app，
   否则 initproc 一启动就找不到自己的子程序。
2. **stride 的整数边界**：`BIG_STRIDE / prio` 在 `prio` 很大时为 0，进程会「永远不涨
   pass」而饿死别人，`.max(1)` 是必须的。
3. **判断 `wait` 的退出码**：子进程 `exit(-2)` 会被内核记成僵尸，`wait` 必须返回它的
   `exit_code`，否则 `Test waitpid OK!` 里对 -2 的断言过不去。
4. **本地 Windows 环境**：并发链接时 `rust-lld` 会偶发 `0xc0000374` 堆损坏，需把
   `CARGO_BUILD_JOBS` 限到 2 后增量重跑。

## 6. 思考题

1. **stride 为什么用 `pass` 累加而不是递减？** `pass` 单调递增便于比较，且与优先级的关系
   是「stride 越小涨得越慢」；若用递减，「剩余预算」在进程数动态变化时很难重新归一化。
2. **`spawn` 与 `fork` + `exec` 的本质差异是什么？** `fork` 复制父进程的整个地址空间
   （页表、堆、栈、fd），再 `exec` 换掉映射；`spawn` 直接为目标 ELF 建一套新映射，
   省掉了复制与销毁的开销，也因此不继承父进程的文件描述符与内存布局。
