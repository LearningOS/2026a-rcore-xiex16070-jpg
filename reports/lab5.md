# 实验五 线程与同步（ch8）

## 1. 实验目的

- 理解「线程 = 共享地址空间 + 独立用户栈 + 独立陷入上下文」，完成 `thread_create`(460)、
  `gettid`(178)、`waittid`(462)；
- 用「阻塞等待 + 唤醒」而非忙等实现互斥锁(463/464/466)、信号量(467/468/470)、条件变量
  (471/472/473)，避免 `sync_sem` / `mpsc_sem` 空转卡死；
- 实现死锁检测（`enable_deadlock_detect` 469）：阻塞前判断「是否还能有进展」，命中返回
  `-0xdead`；
- 实现管道 `pipe`(59) 与 `dup`(24)：环形缓冲、读写端引用与 EOF 语义。

## 2. 实验环境

| 项目 | 内容 |
| --- | --- |
| 代码基线 | `LearningOS/2026a-rcore-xiex16070-jpg` 仓库 `ch8` 分支 |
| 官方检查器 | `rCore-Tutorial-Checker` @ `7d61ec55b58eed6ca7052917846c1b87af34563a` |
| 官方测试集 | `rCore-Tutorial-Test` @ `a0593662ad55d670ba8c27ce1763347cd0dd552f` |
| 工具链 | Rust `nightly-2024-05-02`，目标 `riscv64gc-unknown-none-elf` |
| 运行环境 | `qemu-system-riscv64 -machine virt`；本地 `QEMU 11.1.0`，课程 CI 镜像为 `QEMU 7.0.0` |
| 引导固件 | 仓库自带的 `bootloader/rustsbi-qemu.bin`，换成本仓库的 `rustsbi-mini`（见实验一 §2 说明） |

> 关于引导固件：课程 CI 镜像 `alicesama/rcore-ci@sha256:6e5706f0…` 内的 QEMU 7.0.0 与随仓库
> 分发的 RustSBI Prototyper 0.4.1 不兼容（在 `Boot HART ID` 之后即无输出）。本仓库把五章的
> `bootloader/rustsbi-qemu.bin` 统一替换为自研的 `rustsbi-mini`（源码在 `tools/firmware/`）：
> 只实现内核用到的 `set_timer` / `console_putchar` / `console_getchar` / `system_reset` 四个
> SBI 调用与定时器中断注入，只用 CLINT、NS16550A、SiFive test、PMP 这些各代 QEMU 都有的设备，
> 因此同一份二进制在 QEMU 7.0.0 与 11.x 上都能启动内核。

## 3. 实验内容

### 3.1 线程：共享地址空间、独立用户栈

`thread_create` 传入父线程的 `process`：同进程线程共享 `MemorySet`、fd 表与同步对象，
只有 tid、用户栈（`ustack_base + tid * (PAGE_SIZE + USER_STACK_SIZE)`）和 trap 上下文独立。

```rust
// os/src/syscall/thread.rs（节选）
let new_task = Arc::new(TaskControlBlock::new(Arc::clone(&process), // 同一个 PCB
    task.inner_exclusive_access().res.as_ref().unwrap().ustack_base, true));
process_inner.tasks[new_task_tid] = Some(Arc::clone(&new_task));   // tid 即下标
*new_task_inner.get_trap_cx() = TrapContext::app_init_context(entry,
    new_task_res.ustack_top(), kernel_token(), new_task.kstack.get_top(),
    trap_handler as usize);
```

主线程 tid 为 0；线程退出时保留 `exit_code`，由 `waittid` 取走（`-1` 表示不存在、`-2` 表示
尚未退出，用户态收到 `-2` 后 `yield` 重试，内核不能忙等）。

### 3.2 互斥锁：阻塞等待与所有权交接

`MutexSpin` 自旋时主动 `yield`，`MutexBlocking` 睡眠等待；unlock 若有等待者则直接交棒
（保留 `locked` 为真），`owner` 由被唤醒者补写，供死锁检测使用。

```rust
// os/src/sync/mutex.rs
if mutex_inner.locked {
    mutex_inner.wait_queue.push_back(current_task().unwrap());
    drop(mutex_inner);
    block_current_and_run_next();                       // 睡眠，等 unlock 交棒
    self.inner.exclusive_access().owner = Some(current_tid()); // 醒来后补写 owner
} else {
    mutex_inner.locked = true;
    mutex_inner.owner = Some(current_tid());
}
// unlock：owner = None；有等待者则 wakeup_task 直接交棒，否则 locked = false
```

### 3.3 信号量：计数与持有者

`count >= 0` 表示空闲资源数，负数绝对值为等待者数；`holders` 记录持有者 tid 供死锁检测。

```rust
// os/src/sync/semaphore.rs
pub fn up(&self) {
    let mut inner = self.inner.exclusive_access();
    inner.count += 1;
    if inner.count <= 0 { inner.wait_queue.pop_front().map(wakeup_task); }
    if let Some(pos) = inner.holders.iter().rposition(|t| *t == current_tid()) {
        inner.holders.remove(pos);
    }
}
pub fn down(&self) {
    let mut inner = self.inner.exclusive_access();
    inner.count -= 1;
    if inner.count < 0 {
        inner.wait_queue.push_back(current_task().unwrap());
        drop(inner);
        block_current_and_run_next();
        self.inner.exclusive_access().holders.push_back(current_tid());
    } else {
        inner.holders.push_back(current_tid());
    }
}
```

### 3.4 条件变量：wait 原子地释放锁

```rust
// os/src/sync/condvar.rs
pub fn wait(&self, mutex: Arc<dyn Mutex>) {
    mutex.unlock();                     // 先释放锁，signal 方才能进临界区
    self.inner.exclusive_access().wait_queue.push_back(current_task().unwrap());
    block_current_and_run_next();
    mutex.lock();                       // 被唤醒后重新加锁再返回用户态
}
// signal：从 wait_queue pop_front 后 wakeup_task
```

`ch8_test_condvar` 中线程二在 `A == 0` 时 `wait`，线程一置 `A = 1` 后 `signal`。

### 3.5 死锁检测：可进展性分析

检测不找环，而是判断「当前任务阻塞后，它等的资源是否还有机会被释放」：可运行的任务必然能
进展，定时器/条件变量上的任务迟早被唤醒；互斥锁看唯一持有者，信号量只要任一持有者能进展
即可（取或，而不是要求全部）。

```rust
// os/src/sync/deadlock.rs（节选）
if status != TaskStatus::Blocked { return true; }
let wait_target = match wait_target { Some(t) => t, None => return true };
if !visiting.insert(tid) { return false; }              // 绕回自己：无出口
// 信号量分支：还有空闲资源，或没有持有者，都不算死锁
if sem.count() > 0 { return true; }
let holders = sem.holders();
if holders.is_empty() { return true; }
holders.iter().any(|h| task_can_progress(inner, *h, current_tid, visiting))
```

```rust
// os/src/syscall/sync.rs（sys_mutex_lock 与 sys_semaphore_down 同构）
if detect_enabled && mutex.is_blocking() && mutex.is_locked()
    && detect_deadlock(&process.inner_exclusive_access(), tid,
                       WaitTarget::Mutex(mutex_id)) {
    return -0xdead;      // 用户态收到 -57005
}
```

`enable_deadlock_detect`(469) 在 PCB 上置标志。三条测试分别覆盖：同线程重入阻塞锁（`owner`
是自己，递归恒为「有进展」）；三条线程全链互等（t3 得到 `-0xdead`）；等待成环但环上有可运行
者（按「任一持有者」返回 true，不误报——若按环判定会把 `deadlock_sem2` 误报）。

### 3.6 管道 pipe 与 dup

64 字节环形缓冲，`Full` / `Empty` / `Normal` 三态区分「头尾相等」时是空还是满；写端以
`Weak<Pipe>` 挂在缓冲上，`upgrade()` 失败即所有写端关闭，读端据此返回 EOF。

```rust
// os/src/fs/pipe.rs（节选）
if ring_buffer.available_read() == 0 {
    if ring_buffer.all_write_ends_closed() { return already_read; }  // EOF
    drop(ring_buffer);
    suspend_current_and_run_next();     // 让出 CPU 等写入，而不是忙等
}
// write 侧对称：available_write() == 0 时让出
```

```rust
// os/src/syscall/fs.rs
let (read_end, write_end) = make_pipe();
let read_fd = inner.alloc_fd();  inner.fd_table[read_fd] = Some(read_end);
let write_fd = inner.alloc_fd(); inner.fd_table[write_fd] = Some(write_end);
*translated_refmut(token, pipe) = read_fd;   // pipe.add(1) 写 write_fd
// sys_dup：仅对 fd_table[fd] 做 Arc::clone 并分配新的 fd
```

## 4. 测试结果

`python tools\local_ci.py 8 --timeout 120`（同一份 `check/ch8.py`、测试集、官方
`Makefile-ch6` 与 `easy-fs-fuse-ch7.rs`）：

```
[PASS] found <pipetest passed!>
[PASS] found <mpsc_sem passed!>
[PASS] found <philosopher dining problem with mutex test passed!>
[PASS] found <race adder using spin mutex test passed!>
[PASS] found <sync_sem passed!>
[PASS] found <test_condvar passed!>
[PASS] found <threads with arg test passed!>
[PASS] found <threads test passed!>
[PASS] found <deadlock test mutex 1 OK!>
[PASS] found <deadlock test semaphore 1 OK!>
[PASS] found <deadlock test semaphore 2 OK!>
[PASS] not found <FAIL: T.T>
[PASS] not found <Test sbrk failed!>

Test passed: 25/25
RESULT: PASS
```

23 条必现项 + 2 条禁现项全部符合，`check exit code: 0`；串口最后是 `ch8 Usertests passed!`
与 `[kernel] Idle process exit with exit_code 0`，无内核 panic。`deadlock_sem1` 期间那行
`Deadlock detected. Test 08_sem1 failed!` 是检测成功的**预期**输出，不是失败。

## 5. 遇到的问题

1. **`rust-lld` 偶发 0xc0000374**：用户程序链接报 `exit code: 0xc0000374`（Windows 堆损坏），
   同程序重试即成功，于是把 `CARGO_BUILD_JOBS` 固定为 2。
2. **`easy-fs-fuse` 链接失败**：工作区路径含中文，mingw `ld.exe` 打不开 `.rlib`
   （报 `cannot find ...libcore-*.rlib`，文件其实存在）与 `-lwinapi_advapi32`；改设
   `CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=rust-lld` 后正常。
3. **`virtio-drivers` git 依赖超时**：清掉半成品 `.cargo/git/db` 缓存，配合
   `CARGO_NET_GIT_FETCH_WITH_CLI=true` 重新拉取，之后依赖进缓存。
4. **多个 `local_ci` 并发共用构建树**：`rust-lld` CPU 停在 0.08s 十几分钟、`.elf` 不出；
   改为串行并降低并行度。
5. **死锁检测误报/漏报**：按环判定会误报 `deadlock_sem2`；交棒后不更新 `owner` 又会漏检
   `deadlock_mutex1`；最终用可进展性递归 + 信号量取或。
6. **`waittid` 的 `-2` 语义**：内核不能忙等，返回 `-2` 让用户态 `yield` 重试。

## 6. 思考题

1. **为什么线程共享地址空间，却要有独立的用户栈？**
   共享地址空间让线程看到同一份代码、数据、堆、fd 表与同步对象；但执行流是私有的：每个线程
   有自己的调用帧、局部变量、返回地址，还要有独立 `TrapContext` 保存寄存器现场。共用栈会让
   栈帧互相覆盖，因此用户栈与 trap 上下文按 tid 错开；切换同进程线程时 `satp` 不变，变的只是
   `sp` 与 TrapContext。
2. **为什么条件变量必须和互斥锁配合使用？**
   「检查条件」与「进入睡眠」必须原子，否则会丢失唤醒：线程判断 `A == 0` 成立、尚未挂入等待
   队列，另一线程置 `A = 1` 并 `signal`（此时队列为空，信号丢失），前者随后才睡下，永远等不到
   第二次 `signal`。`wait` 先释放锁再入队阻塞、被唤醒后重新加锁，可保证 `signal` 要么发生在检查
   之前（条件已满足，不睡），要么发生在入队之后（一定能被唤醒）；用户态用 `while` 重查条件还能
   兼容虚假唤醒。
