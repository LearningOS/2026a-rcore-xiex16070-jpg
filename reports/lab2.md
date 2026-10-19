# 实验二 虚拟内存映射（ch4）

## 1. 实验目的

- 理解 RISC-V Sv39 三级页表的建立与切换过程，掌握「内核通过页表访问用户内存」的正确姿势；
- 在 ch4 的分页机制上实现 `mmap` / `munmap` 两个系统调用，支持按页申请、释放用户虚拟内存；
- 修正 `sbrk` 的边界语义，并把 ch3 的 `trace` 迁移到「经页表审计」的访存路径上。

## 2. 实验环境

| 项目 | 内容 |
| --- | --- |
| 代码基线 | `LearningOS/2026a-rcore-xiex16070-jpg` 仓库 `ch4` 分支 |
| 官方检查器 | `rCore-Tutorial-Checker` @ `7d61ec55b58eed6ca7052917846c1b87af34563a` |
| 官方测试集 | `rCore-Tutorial-Test` @ `a0593662ad55d670ba8c27ce1763347cd0dd552f` |
| 工具链 | Rust `nightly-2024-05-02`，目标 `riscv64gc-unknown-none-elf` |
| 运行环境 | `qemu-system-riscv64 -machine virt`，`qemu 7.0.0` 及以上 |

## 3. 实验内容

ch4 的内核里，用户程序的每个字节都不再能「直接解引用」：必须拿着当前进程的
`satp`（`current_user_token()`）查页表，找到物理页帧后再读写。本实验新增的三件事都
围绕这条规则展开。

### 3.1 带权限审计的字节缓冲

模板里的 `translated_byte_buffer` 只做地址翻译，不检查 PTE 的读写权限。`mmap` 允许建立
「只读」映射，此时 `trace_write` 必须失败，因此新增 `UserAccess` 与受检版本：

```rust
// os/src/mm/page_table.rs
pub const USER_ADDR_LIMIT: usize = 1 << 38; // Sv39 用户空间上界

pub enum UserAccess {
    Read,
    Write,
}

impl PageTable {
    fn allowed(&self, pte: &PageTableEntry) -> bool {
        match self.access {
            UserAccess::Read => pte.readable(),
            UserAccess::Write => pte.writable(),
        }
    }

    pub fn translated_byte_buffer_checked(
        token: usize,
        ptr: *const u8,
        len: usize,
        access: UserAccess,
    ) -> Option<Vec<&'static mut [u8]>> { /* 逐页查表，权限不符即返回 None */ }
}
```

这里有两个容易忽略的细节：

1. 先判断 `ptr as usize + len` 是否越过 `USER_ADDR_LIMIT`，再做地址截断。否则
   `isize::MAX` 这类「看起来是用户地址」的指针会绕回内核的 trampoline 页，把内核数据当成
   用户内存读写。
2. 遇到第一页就失败时要立刻返回 `None`，不能返回「部分成功」的缓冲——系统调用的语义是
   要么整体完成，要么返回 `-1`。

### 3.2 `mmap_area` / `munmap_area`

`mmap` 在 `MemorySet` 里对应「新建一个 `Framed` 的 `MapArea`」，`munmap` 对应「把一个
`MapArea` 从中间挖掉一段」。两者都要做参数与重叠检查：

```rust
// os/src/mm/memory_set.rs
pub fn mmap_area(&mut self, start_va: VirtAddr, len: usize, permission: MapPermission) -> bool {
    let end_va = match start_va.0.checked_add(len) {
        Some(end_va) if len > 0 => end_va,
        _ => return false,
    };
    let start_vpn = start_va.floor();
    let end_vpn = VirtAddr::from(end_va).ceil();
    let overlap = self.areas.iter().any(|area| {
        area.vpn_range.get_start() < area.vpn_range.get_end()
            && area.vpn_range.get_start() < end_vpn
            && start_vpn < area.vpn_range.get_end()
    });
    if overlap {
        return false;
    }
    self.push(MapArea::new(start_va, VirtAddr::from(end_va), MapType::Framed, permission), None);
    true
}
```

`munmap_area` 要求被释放的区间完整落在**同一个** `MapArea` 内；随后逐页 `unmap`，用
`data_frames.split_off` 把中间那段页帧从 `BTreeMap` 里摘出来交给 `FrameTracker` 的 `Drop`
回收，最后按需把左右两段重新 `push` 回 `areas`。这样「挖中间」不会误伤两端的页帧。

### 3.3 三个系统调用的参数校验

```rust
// os/src/syscall/process.rs
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    if start % PAGE_SIZE != 0 || len == 0 {
        return -1;
    }
    if prot & !0b111 != 0 || prot & 0b111 == 0 { // 只允许低 3 位且不能为 0
        return -1;
    }
    match start.checked_add(len) {
        Some(end) if end <= USER_ADDR_LIMIT => {}
        _ => return -1,
    }
    let permission = MapPermission::U
        | MapPermission::R * (prot & 0b001 != 0) // 逐位拼接 R/W/X
        | MapPermission::W * (prot & 0b010 != 0)
        | MapPermission::X * (prot & 0b100 != 0);
    if mmap(VirtAddr::from(start), len, permission) { 0 } else { -1 }
}

pub fn sys_munmap(start: usize, len: usize) -> isize {
    if start % PAGE_SIZE != 0 || len % PAGE_SIZE != 0 || len == 0 {
        return -1;
    }
    if munmap(VirtAddr::from(start), len) { 0 } else { -1 }
}
```

注意 `mmap` 的 `len` **不要求**页对齐（`0x1000 + 1` 也合法，按向上取整映射），而
`munmap` 的 `start` 与 `len` 都必须页对齐——这正是测试 04_4 与 04_6 的分界。

`sbrk` 沿用模板的 `change_program_brk`：它通过 `shrink_to` / `append_to` 调整数据段的
`MapArea`，失败（越界、与其它区域冲突、缩到起点之前）时返回 `None`，再由 `sys_sbrk`
翻译成 `-1`。

### 3.4 `trace` 与 `sys_get_time` 迁移到受检路径

ch3 的 `trace` 在 ch4 必须改成「查页表再读写」：

```rust
fn read_user_byte(addr: usize) -> Option<u8> {
    translated_byte_buffer_checked(current_user_token(), addr as *const u8, 1, UserAccess::Read)?
        .first().map(|buffer| buffer[0])
}
```

于是 `trace_read(0x80200000)`（内核地址，用户页表里没有映射）返回 `None`、
对只读映射的 `trace_write` 返回 `-1`、`munmap` 之后同一地址的 `trace_read` 又变回 `None`，
与测试 `ch4_trace1` 的断言一一对应。`sys_get_time` 同样改为先取受检缓冲再写入
`TimeVal`，从而支持跨页的时间结构体。

## 4. 测试结果

本地按官方 CI 的方式复现（同一份 checker、同一份测试集、官方 `overwrite.py` 与
`Makefile-ch4`），`check/ch4.py` 输出：

```
[PASS] found <get_time OK! (\d+)>
[PASS] found <Test sleep OK!>
[PASS] found <current time_msec = (\d+)>
[PASS] found <time_msec = (\d+) after sleeping (\d+) ticks, delta = (\d+)ms!>
[PASS] found <Test sleep1 passed!>
[PASS] found <string from task trace test>
[PASS] found <Test trace OK!>
[PASS] found <Test 04_1 OK!>
[PASS] found <Test 04_4 test OK!>
[PASS] found <Test 04_5 ummap OK!>
[PASS] found <Test 04_6 ummap2 OK!>
[PASS] found <Test trace_1 OK!>

Test passed: 12/12
```

同时 4 条禁止出现的模式也都没有出现：`FAIL: T.T`、`Test sbrk failed!`、
`Should cause error, Test 04_2 fail!`、`Should cause error, Test 04_3 fail!`。21 个用户程序
全部跑完，串口最后一行是 `[kernel] Panicked at src/task/mod.rs ... All applications completed!`。

## 5. 遇到的问题

1. **`isize::MAX` 被当成合法用户地址**：只做 `vpn < USER_ADDR_LIMIT` 的检查时，
   `isize::MAX as *const u8` 截断后仍可能命中内核 trampoline 页，导致 `trace_read` 返回
   `Some(..)` 而不是 `None`。修法是先判原始指针是否已经越过 `USER_ADDR_LIMIT`。
2. **`munmap` 挖中间导致两端丢失**：最初直接把整段 `MapArea` 移除再重建，右半段页帧被
   提前释放，`munmap` 之后访问相邻页会触发 page fault。改用
   `data_frames.split_off` 只摘走被释放的那一段。
3. **`mmap` 与已有区域重叠**：不做重叠检查时，`mmap` 会把已有的 `MapArea` 覆盖掉但页表
   仍是旧的 PTE，表现为「映射成功却访问到旧数据」。加 `overlaps` 判断后返回 `-1`。

## 6. 思考题

**为什么 `mmap` 的 `len` 可以不页对齐，`munmap` 却必须页对齐？**
`mmap` 的语义是「至少给我这么多字节」，向上取整到整页是天然的（多余部分仍可用）；
而释放是按虚拟页回收的，若允许非对齐的 `len`，被释放的边界会落在页中间，无法决定这一页
是否该整体回收，所以要求调用者显式页对齐，语义清晰且不会误释放。

**为什么 `trace` 读写用户内存必须走页表，而不能像 ch3 那样直接解引用？**
ch4 起用户地址是虚拟地址，物理帧由页表决定；直接解引用会读到内核自己的地址空间
（`satp` 已切到用户页表，物理地址不再等价于虚拟地址），既拿不到用户数据，也会把野指针
变成内核崩溃。走页表还能顺带检查 PTE 的 R/W 权限，实现「只读映射不可写」。
