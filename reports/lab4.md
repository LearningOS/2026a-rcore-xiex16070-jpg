# 实验四 文件系统与管道（ch6）

## 1. 实验目的

- 在 easy-fs 上补齐**硬链接**语义：磁盘 inode 增加链接计数 `nlink`，实现 `link` / `unlink`；
- 把文件和进程的 `fstat`（`Stat` 结构）接通到用户态，理解 `OSInode` 与 `Inode` 的分层；
- 让文件系统与「进程 + 地址空间」协同工作：`spawn`、`mmap` / `munmap`、`get_time` 在 ch6 里
  都要经过独立的 `MemorySet`；
- 观察 `easy-fs-fuse` 如何把用户程序打包成 `fs.img`，并通过 virtio-blk 交给内核挂载。

## 2. 实验环境

| 项目 | 内容 |
| --- | --- |
| 代码基线 | `LearningOS/2026a-rcore-xiex16070-jpg` 仓库 `ch6` 分支 |
| 官方检查器 | `rCore-Tutorial-Checker` @ `7d61ec55b58eed6ca7052917846c1b87af34563a` |
| 官方测试集 | `rCore-Tutorial-Test` @ `a0593662ad55d670ba8c27ce1763347cd0dd552f` |
| 工具链 | Rust `nightly-2024-05-02`，目标 `riscv64gc-unknown-none-elf` |
| 运行环境 | `qemu-system-riscv64 -machine virt`，`qemu 7.0.0` 及以上，virtio-blk 挂载 `fs.img` |

## 3. 实验内容

### 3.1 磁盘 inode 的链接计数

```rust
// easy-fs/src/layout.rs
pub struct DiskInode {
    pub size: u32,
    pub direct: [u32; INODE_DIRECT_COUNT],
    pub indirect1: u32,
    pub indirect2: u32,
    pub nlink: u32,          // 新增：硬链接数
    type_: DiskInodeType,
}

impl DiskInode {
    pub fn initialize(&mut self, type_: DiskInodeType) {
        self.size = 0;
        self.direct.iter_mut().for_each(|v| *v = 0);
        self.indirect1 = 0;
        self.indirect2 = 0;
        self.nlink = 1;      // 新建即被目录项引用一次
        self.type_ = type_;
    }
}
```

`DiskInode` 必须仍能塞进 128 字节的 inode 区（`INODE_SIZE`），加一个 `u32` 后 `size` 仍是
100 字节，安全；但这一点必须核对，否则 `get_block_cache(...).read/modify` 会越界写坏相邻
inode。

### 3.2 `Inode` 上的 link / unlink / nlink

```rust
// easy-fs/src/vfs.rs
pub fn inode_id(&self) -> u32 { self.inode_id }
pub fn is_dir(&self) -> bool { self.read_disk_inode(|d| d.is_dir()) }
pub fn nlink(&self) -> u32 { self.read_disk_inode(|d| d.nlink) }

/// 在目录 self 里为 target 再挂一个名字 name
pub fn link(&self, name: &str, target: &Inode) -> Option<()> {
    let mut fs = self.fs.lock();
    // 名字已存在则失败，保证目录项不重复
    if self.read_disk_inode(|d| self.find_inode_id(name, d)).is_some() {
        return None;
    }
    self.modify_disk_inode(|disk_inode| {
        let file_count = (disk_inode.size as usize) / DIRENT_SZ;
        // 追加一个 DirEntry { name, inode_id: target.inode_id }
        // 必要时分配新的数据块并把 size 扩大
    });
    target.modify_disk_inode(|d| d.nlink += 1);
    Some(())
}
```

`unlink` 是镜像操作：在目录里找到 `name`，把**最后一个目录项搬过来覆盖它**（避免留下空洞）、
`size -= DIRENT_SZ`，再对目标 inode `nlink -= 1`；只有 `nlink` 归零时才真正释放数据块。
这就是「硬链接」与「删除文件」的区别：`unlink` 删的是一个名字，inode 直到最后一个名字消失
才回收。

### 3.3 fstat：OSInode 视角的 Stat

```rust
// os/src/fs/inode.rs
pub fn get_stat(&self) -> Stat {
    let inner = self.inner.exclusive_access();
    let inode = &inner.inode;
    Stat {
        dev: 0,
        ino: inode.inode_id() as u64,
        mode: if inode.is_dir() { StatMode::DIR } else { StatMode::FILE },
        nlink: inode.nlink() as u32,
        // uid/gid/size/... 其余字段按 easy-fs 的信息填
    }
}
```

```rust
// os/src/syscall/fs.rs
pub fn sys_fstat(fd: usize, st: *mut Stat) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();
    let inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() { return -1; }
    if let Some(file) = &inner.fd_table[fd] {
        if let Some(stat) = file.stat() {
            *translated_refmut(token, st) = stat;   // 经页表写到用户内存
            0
        } else { -1 }
    } else { -1 }
}
```

`fstat` 拿到的 `Stat` 必须先在内核栈上构造完整，再一次性写入用户地址，不能把裸指针
交给 easy-fs。

### 3.4 linkat / unlinkat

```rust
pub fn sys_linkat(old_name: *const u8, new_name: *const u8) -> isize {
    let token = current_user_token();
    let old_name = translated_str(token, old_name);
    let new_name = translated_str(token, new_name);
    link_file(&old_name, &new_name)     // 目录不可硬链接，由 link_file 拒绝
}
pub fn sys_unlinkat(name: *const u8) -> isize {
    unlink_file(&translated_str(token, name))
}
```

### 3.5 进程相关系统调用的回归

ch6 的 usertest 会重跑 ch4/ch5 的部分用例，因此 `sys_get_time`、`sys_mmap`、`sys_munmap`、
`sys_spawn` 以及 `TaskControlBlock::spawn`、`MemorySet::munmap` / `is_overlapped` 都需要在
ch6 的代码基线上重新实现一遍（ch6 分支的模板与 ch5 分支并不共享这些改动）：

```rust
// os/src/task/task.rs
pub fn spawn(self: &Arc<TaskControlBlock>, elf_data: &[u8]) -> Arc<TaskControlBlock> {
    let (memory_set, user_sp, entry_point) = MemorySet::from_elf(elf_data);
    let trap_cx_ppn = memory_set
        .translate(VirtAddr::from(TRAP_CONTEXT_BASE).into()).unwrap().ppn();
    let pid_handle = pid_alloc();
    let kernel_stack = kstack_alloc();
    // 组装 TCB，把 fd_table 从当前进程复制过去（spawn 继承父进程打开的 fd）
    ...
}
```

## 4. 测试结果

`python tools\local_ci.py 6 --timeout 150`：

```
[PASS] found <Test file0 OK!>
[PASS] found <Test fstat OK!>
[PASS] found <Test link OK!>
[PASS] found <Test mass open/unlink OK!>
...
Test passed: 31/31
```

串口日志（464 行）中 initproc 依次拉起 ch2b/ch3/ch4/ch5/ch6 的全部用例，
`ch4b_sbrk`、`ch4_mmap1`、`ch4_mmap2` 以 -2 退出是测试**故意**触发的缺页/非法访存
（内核打印 `StorePageFault`/`LoadPageFault ... kernel killed it.`），属于预期；
`ch6_file0` / `ch6_file3` 分别验证了 `file_test passed!` 与 `Test mass open/unlink OK!`，
最后打印 `ch6 Usertests passed!`。

## 5. 遇到的问题

1. **`DiskInode` 的 128 字节上限**：easy-fs 的 `get_block_cache().read/modify` 直接按
   `DiskInode` 的大小读写，加字段必须先确认 `size_of::<DiskInode>() <= INODE_SIZE`。
2. **目录项删除不能留洞**：`unlink` 用「末项覆盖」而不是把 `inode_id` 清 0，否则后续
   `link` 的 `file_count` 计算会与真实目录长度脱节，`Test mass open/unlink OK!` 会失败。
3. **`easy-fs-fuse` 是宿主程序**：本仓库路径含中文，mingw 的 `ld.exe` 打不开，必须设
   `CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=rust-lld`；且 checker 每次都会覆盖
   `easy-fs-fuse/src/main.rs`，时间戳变化导致必然重新链接，所以每次跑 ch6 都要带着这个环境变量。
4. **Windows 上 QEMU 偶发早退**：曾在 `ch6_file3` 第 5 轮后无 panic 静默结束（30/31），
   同一 `fs.img` 手工重跑即恢复，属于宿主机抖动，重跑即可。

## 6. 思考题

1. **为什么 `unlink` 不能立刻删除 inode？** 因为同一个 inode 可能被多个目录项（甚至多个
   进程的 fd）引用；`nlink` 记录的是「名字」的数量，进程的 `fd` 还额外持有一份引用。
   立刻删除会让仍打开该文件的进程读到已释放的数据块。
2. **`fstat` 为什么放在 `OSInode` 而不是 `Inode` 上？** `Inode` 只认识 easy-fs 的磁盘结构，
   而 `Stat` 是「内核向用户暴露的抽象」：它还包含打开模式、读写偏移等 `OSInode` 才有的
   信息；分层后 easy-fs 可以独立于内核的 `Stat` 定义演进。
