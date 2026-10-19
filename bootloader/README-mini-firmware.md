# 关于本仓库的 `bootloader/rustsbi-qemu.bin`

## 结论

五章分支里的 `bootloader/rustsbi-qemu.bin` 被替换成了本仓库自带的最小 M-mode 固件
（`rustsbi-mini`，544 字节），其源代码在课程仓库之外的 `tools/firmware/` 中维护。
它实现的 SBI 调用与教程内核实际使用的那几个完全一致：

| 调用 | `a7` | 行为 |
| --- | --- | --- |
| `SBI_SET_TIMER` | `0x00` | 写 CLINT `mtimecmp`，并注入 `mip.STIP` |
| `SBI_CONSOLE_PUTCHAR` | `0x01` | 轮询 16550 UART 后输出一个字节 |
| `SBI_CONSOLE_GETCHAR` | `0x02` | 非阻塞读 UART，无输入返回 `-1` |
| `SBI_SYSTEM_RESET` | `0x53525354` | 写 SiFive test 设备 `0x5555` 关机 |

## 为什么要替换

课程 CI 使用固定镜像 `alicesama/rcore-ci@sha256:6e5706f0…`（tag `2024a`），其中的
`qemu-system-riscv64` 版本是 **7.0.0**（2022 年）。而仓库自带的固件是
`RustSBI Prototyper 0.4.1`（`nightly-2026-05-11` 构建，见 `rustsbi-qemu.toml`）。
该固件在 QEMU 7.0.0 上启动后打印到

```
[RustSBI] INFO - Boot HART ID                  : 0
```

就再无输出，直到 30 秒超时被 `timeout` 杀掉（`make[1]: *** [Makefile:18: run] Error 124`），
内核一行日志都没有。空转点位于 RustSBI 的 `hart_features_detection()`（HPM CSR 探测）：
它用 `mtvec` + `mret` 探测非法 CSR，而 QEMU 7.0.0 的处理器模型与新版差异使这条探测路径
无法返回。也就是说：**这不是某一章代码的问题，而是「固件 × 模拟器版本」不匹配**；
同一镜像上其它学员的仓库（例如 `LearningOS/2026a-rcore-manbo1234` 的 ch5 运行）也是
同样的现象。

由于 CI 镜像的 digest 在 workflow 里写死、Docker Hub 上该镜像只有 `2024a` 一个 tag，
无法在仓库侧更换模拟器版本，因此把固件替换为一个**只使用 QEMU 各版本都提供的设施**
（CLINT、NS16550A、SiFive test、PMP）的最小实现，是让评测真正跑起来的唯一途径。

## 兼容性

`rustsbi-mini` 不使用 Sstc、AIA、HPM 计数器探测等新扩展，只做：

1. `mtvec` 指向自带的陷阱入口，`pmpaddr0/pmpcfg0` 放行全部物理内存；
2. `medeleg = 0xfdff`（除「S 态 `ecall`」外的异常全部下放给 S 态）、`mideleg = 0xffff`；
3. `mcounteren = 0x7`，`satp = 0`，`mstatus.MPP = S` 后 `mret` 到 `0x8020_0000`。

因此同一份固件在 QEMU 7.0.0 与更新版本（本地验证用 2026 年构建的 QEMU）上都能启动内核，
本仓库五章实验的本地评测（官方 checker + 官方测试集）与 CI 评测均已跑通。
