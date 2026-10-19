# 实验一 系统调用 trace（ch3）

## 1. 实验目的

- 理解 RISC-V 下用户态与内核态之间的切换过程，以及 `ecall` 触发的系统调用分发路径；
- 在 ch3 的任务管理框架上实现 `trace` 系统调用：统计当前进程发起的系统调用次数，并支持读写用户地址空间中的内存；
- 熟悉 rCore-Tutorial v3 的内核结构：`trap`、`task`、`syscall` 三个模块的协作关系。

## 2. 实验环境

| 项目 | 内容 |
| --- | --- |
| 代码基线 | `LearningOS/2026a-rcore-xiex16070-jpg` 仓库 `ch3` 分支 |
| 官方检查器 | `rCore-Tutorial-Checker` @ `7d61ec55b58eed6ca7052917846c1b87af34563a` |
| 官方测试集 | `rCore-Tutorial-Test` @ `a0593662ad55d670ba8c27ce1763347cd0dd552f` |
| 工具链 | Rust `nightly-2024-05-02`，目标 `riscv64gc-unknown-none-elf` |
| 运行环境 | `qemu-system-riscv64 -machine virt`，`qemu 7.0.0` 及以上 |
| 引导固件 | 仓库自带 `rustsbi-mini`（544 字节，源码见 `tools/firmware/`，说明见 `bootloader/README-mini-firmware.md`） |

> 说明：课程 CI 镜像 `alicesama/rcore-ci:2024a` 内的 QEMU 是 7.0.0，仓库原带的
> RustSBI Prototyper 0.4.1 固件在该版本的 `hart_features_detection()`（HPM CSR 探测）
> 处空转，内核一行日志都不会输出。因此把固件换成只使用 CLINT、NS16550A、SiFive test、
> PMP 这些各版本 QEMU 都提供的设施的最小实现，SBI 语义与内核实际使用的四个调用一致。

## 3. 实验内容

`trace` 系统调用（编号 410）有三个参数：`trace_request`、`id`、`data`，其中
`trace_request` 取值 0/1/2 分别表示读用户内存、写用户内存、查询系统调用次数：

```rust
pub enum TraceRequest {
    Read,
    Write,
    Syscall,
}
```

### 3.1 系统调用计数的存储

每个任务需要一张「系统调用编号 → 次数」的表。ch3 的任务控制块 `TaskControlBlock`
以 `[TaskControlBlock; MAX_APP_NUM]` 的形式整体拷贝初始化，若把计数表直接放进 TCB，
`16 × 512 × 4B = 32KB` 的临时对象会落在只有 64KB 的内核引导栈（`boot_stack`）上，
存在栈溢出风险，因此把计数表放进独立的静态数组，编译期零初始化进 `.bss`：

```rust
// os/src/config.rs
pub const MAX_SYSCALL_NUM: usize = 512;

// os/src/task/mod.rs
static mut SYSCALL_COUNTS: [[u32; MAX_SYSCALL_NUM]; MAX_APP_NUM] =
    [[0; MAX_SYSCALL_NUM]; MAX_APP_NUM];

/// 记录当前任务又发起了一次编号为 id 的系统调用
pub fn inc_current_syscall_count(id: usize) {
    if id >= MAX_SYSCALL_NUM {
        return;
    }
    let current = TASK_MANAGER.inner.exclusive_access().current_task;
    unsafe {
        SYSCALL_COUNTS[current][id] += 1;
    }
}

/// 当前任务发起编号为 id 的系统调用的次数
pub fn current_syscall_count(id: usize) -> usize {
    if id >= MAX_SYSCALL_NUM {
        return 0;
    }
    let current = TASK_MANAGER.inner.exclusive_access().current_task;
    unsafe { SYSCALL_COUNTS[current][id] as usize }
}
```

### 3.2 计数的时机

计数放在系统调用分发之前，这样「查询自己调用了几次」的 `trace` 调用本身也会被统计进去：

```rust
// os/src/syscall/mod.rs
pub fn syscall(syscall_id: usize, args: [usize; 3]) -> isize {
    // 先记账再分发，保证 trace(Syscall, id) 能看到本次调用自身
    crate::task::inc_current_syscall_count(syscall_id);
    match syscall_id {
        SYSCALL_WRITE => sys_write(args[0], args[1] as *const u8, args[2]),
        SYSCALL_EXIT => sys_exit(args[0] as i32),
        SYSCALL_YIELD => sys_yield(),
        SYSCALL_GET_TIME => sys_get_time(args[0] as *mut TimeVal, args[1]),
        SYSCALL_TRACE => sys_trace(args[0], args[1], args[2]),
        _ => panic!("Unsupported syscall_id: {}", syscall_id),
    }
}
```

### 3.3 用户内存的读写

ch3 尚未开启分页，内核与用户程序共用同一个物理地址空间：应用被加载到
`APP_BASE_ADDRESS = 0x8040_0000` 起的连续区域，用户栈则是内核 `.bss` 中的静态数组。
因此内核可以直接按地址读写用户内存，只需要把地址限制在物理内存区间
`[0x8000_0000, MEMORY_END)` 内，避免解引用非法指针把内核打崩：

```rust
const MEMORY_START: usize = 0x8000_0000;

fn is_valid_user_addr(addr: usize) -> bool {
    (MEMORY_START..MEMORY_END).contains(&addr)
}

pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    match trace_request {
        TRACE_READ => {
            if !is_valid_user_addr(id) {
                return -1;
            }
            unsafe { core::ptr::read_volatile(id as *const u8) as isize }
        }
        TRACE_WRITE => {
            if !is_valid_user_addr(id) {
                return -1;
            }
            unsafe { core::ptr::write_volatile(id as *mut u8, data as u8) };
            0
        }
        TRACE_SYSCALL => current_syscall_count(id) as isize,
        _ => -1,
    }
}
```

至此 `trace` 的三种请求全部实现：读用户字节返回字节值、失败返回 `-1`；写用户字节成功返回
`0`；查询系统调用次数返回 `id` 对应计数。

## 4. 测试结果

在本地按官方 CI 的方式复现：把学生代码放到 `ci-user/os`、官方测试集放到 `ci-user/user`，
应用官方 `overwrite.py` 与 `Makefile-ch3`，构建用户程序与内核后在 QEMU 中运行，
再用官方 `check/ch3.py` 解析串口日志。结果为 **7/7**：

```
[PASS] found <get_time OK! (\d+)>
[PASS] found <Test sleep OK!>
[PASS] found <current time_msec = (\d+)>
[PASS] found <time_msec = (\d+) after sleeping (\d+) ticks, delta = (\d+)ms!>
[PASS] found <Test sleep1 passed!>
[PASS] found <string from task trace test>
[PASS] found <Test trace OK!>

Test passed: 7/7
```

用户程序侧的关键断言也全部成立：

- `3 <= count_syscall(SYSCALL_GETTIMEOFDAY)`、`5 <= count_syscall(SYSCALL_GETTIMEOFDAY)`；
- `count_syscall(SYSCALL_TRACE)` 依次为 2 和 7（与「本次调用自身也计入」的语义一致）；
- `println!` 产生 2 次 `write`；
- `trace_read(&var) == Some(111)`、`trace_write(&var, xor)` 后 `read_volatile` 读到同一个值、
  `trace_read(main)` 返回 `Some(..)` 而非 `None`。

## 5. 遇到的问题

1. **计数时机差一**：若在系统调用处理完之后才累加计数，`trace(Syscall, id)` 查到的次数会比测试
   期望少 1（测试用 `assert_eq!(2, ...)` 与 `assert_eq!(7, ...)` 固定了「本次调用计入」的语义）。
   把累加放在 `syscall()` 分发之前即可。
2. **计数必须按任务隔离**：ch3 采用时钟中断抢占式轮转，测试程序在 `sleep(500)` 循环中被切走，
   其它应用的系统调用不能计入本任务，否则 `assert_eq!(2, count_syscall(WRITE))` 会失败。

## 6. 思考题

**为什么 `println!` 对应两次 `write`？**
用户库的 stdout 是行缓冲：`ConsoleBuffer::write_str` 每遇到 `'\n'` 或缓冲区写满就调用一次
`write`。`println!("string from task trace test\n")` 的格式串是
`"string from task trace test\n\n"`（`println!` 会自动再补一个换行），于是两次换行各触发一次
`write`，共 2 次。

**计数表为什么不用全局一张表？**
任务之间会被时钟中断打断并切换，全局表无法区分调用者；
`trace` 的语义是「当前进程发起某系统调用的次数」，必须按任务（ch5 之后按进程）隔离。
