# 字节码性能基线

本页记录优化前的编译器与 VM；不包含优化后的对照数据。被测实现基于 `0667d4b3f0b0af094e283b37c664726a9014264c`，基准、夹具及采样结果与本页一同提交。`build_id` 记录在两个计时批次及诊断输出中，可以确认三次运行使用了相同的字节码。

## 复现

```sh
env -u YIELD_VM_BENCH_DIAGNOSTICS cargo bench --locked -p yield-harness --bench baseline -- --samples 9 > timings-a.csv
env -u YIELD_VM_BENCH_DIAGNOSTICS cargo bench --locked -p yield-harness --bench baseline -- --samples 9 > timings-b.csv
YIELD_VM_BENCH_DIAGNOSTICS=1 cargo bench --locked -p yield-harness --bench baseline > counters.csv
```

- 默认 Cargo bench profile（继承 release）。计时用两个独立进程，每组负载每进程预跑 2 次后保留 9 个样本。计数单独编译运行，同样的字节码及输入，每阶段一个样本；普通构建不启用计数。不要对诊断构建计时。
- 每个计时样本都先测离线编译和 JSON 编码，再从已生成的内存字节解码。不计文件 I/O，不刷新进程外缓存。`startup_prepare` 包含校验、宿主链接、派生元数据及 VM 初始化准备；`startup_initialize` 包含模块实例化、求值和初始化微任务。`entry_first` 使用新 VM；`entry_steady_5` 是同一 VM 连续再运行 5 次的总时长，包含出参校验及固定输入下的宿主驱动。React 使用测试宿主的虚拟时钟，不等待墙钟。它测量整次提交路径，不是纯解释器循环。
- 最新基准还输出 `startup_build_identity`（版本检查及 JSON 序列化 / SHA-256 摘要）、`startup_build_structure`（字节码结构校验）和 `startup_link_and_setup`（校验后的宿主链接、派生元数据及 VM 准备）；三项之和是 `startup_prepare`。这些子项不在本页的历史 CSV 中；首轮分项见[加载实验](bytecode-loading.md)。
- `startup_build_digest_json` 和 `startup_build_digest_sha256` 是 `startup_build_identity` 内的两个区间：前者把摘要输入序列化到内存，后者只对这些字节计算 SHA-256。两者不含版本检查、缓冲区释放、摘要字符串格式化或 ID 比较，也不等同于产物的编码和解码。
- 挂起负载另用新 VM 构造可恢复状态。单独计初次挂起、捕获、JSON 编解码、恢复、宿主 operation 重挂，以及恢复后的续跑校验；不把外部等待计入任何阶段。`restore` 包括重新校验构建，但不包含恢复输入的克隆或解码。
- `frame_allocations` 是帧构造次数（零槽帧不分配槽缓冲区）；`frame_slots` 是这些帧的总槽数。环境计数是查找 owner 时访问的环境数；属性计数是自有属性探测次数，`property_slow_paths` 是进入通用属性原型链读取的次数，不是缓存失效率。`saved_values / suspensions` 是捕获平均值，分母包含 `await`、`yield` 和生成器初始捕获。
- 峰值 RSS 用未插桩的独立进程采集：先用 `cargo bench --locked -p yield-harness --bench baseline --no-run` 获取可执行文件路径，再对每个工作负载执行 `/usr/bin/time -l <可执行文件> --compile-only <负载名>`（macOS）。RSS 包括进程启动、编译、打印构建 ID，不包含 VM 执行和独立的产物编码。Linux 可用 `/usr/bin/time -v`，注意其 RSS 单位不同。

## 首轮结果

环境：Apple M2（8 核、24 GiB），macOS 27.0.1（26A434），`aarch64-apple-darwin`，`rustc 1.99.1`（`58cf8f9c3`，LLVM 23.1.1）、Cargo 1.99.1。`Cargo.lock` SHA-256：`a526164823005f68ccdbb8ead6b1c1cec17f4fe526b662e18c1d23c3721becb9`。

原始样本：[计时 A](performance-baseline/timings-a.csv)、[计时 B](performance-baseline/timings-b.csv)、[诊断计数](performance-baseline/counters.csv)、[编译峰值 RSS](performance-baseline/compile-rss.csv)。计时 CSV 使用 ns；以下为各批中位数，单位 µs。稳态列将每个样本的 5 次总时长除以 5；React 每次更新负载执行 8 次独立提交。

| 工作负载 | 首次 A / B | 稳态每次 A / B |
| --- | ---: | ---: |
| React 首提交 | 499.17 / 501.92 | 440.66 / 439.89 |
| React 更新 | 2814.58 / 2808.83 | 2242.43 / 2235.50 |
| 数值/分支 | 961.46 / 945.96 | 959.52 / 958.38 |
| 短调用 | 278.38 / 288.29 | 281.99 / 280.16 |
| 闭包读写 | 420.83 / 404.96 | 433.02 / 434.30 |
| 静态属性 | 372.75 / 355.92 | 349.48 / 343.39 |
| `await` 成功 | 19.54 / 19.67 | 16.98 / 16.97 |
| `finally` 中 `await` 拒绝 | 6.29 / 6.21 | 3.92 / 3.93 |
| 同步生成器 | 12.50 / 12.04 | 8.36 / 8.32 |
| 异步生成器 | 18.33 / 17.96 | 13.91 / 13.81 |

- **波动**：稳态中位数跨批最大偏差 1.7%（静态属性）；该负载 5 次总时长的样本范围 A 为 1714–1763 µs、B 为 1693–1736 µs。首次执行中位数最大偏差 4.5%。这些是本机两批的观测值，不是通用回归阈值。
- **React 启动**：以首提交负载 A 批为例，4,614,572 B 产物的离线编译约 120.10 ms、编码 5.12 ms；解码 17.06 ms、启动准备 19.11 ms、模块初始化 1.59 ms。搬走编译后，解码与准备仍是这份产物的主要启动开销；准备阶段的聚合值不能说明其中哪一步最贵。
- **恢复**：四个挂起负载的快照为 302,664–305,163 B。A 批各负载中位数：捕获 143–147 µs，编码 338–345 µs，解码 703–713 µs，恢复 541–677 µs，恢复后首次续跑 1.4–16.9 µs。先区分镜像处理和脚本执行；减少一两个保存槽不一定能改变端到端恢复延迟。
- **诊断计数**：稳态 5 次中，静态属性负载为 66,640 次分派、15,365 次属性探测、5 次帧构造；短调用为 5,125 次帧构造；闭包读写是 20,500 次环境探测。React 更新为 220,875 次分派、29,600 次属性探测、52,410 次环境探测。这些次数刻画工作量，不代表 CPU 时间占比。四个挂起负载初次构造镜像时分别保存 0、2、1、3 个槽值（含初次生成器捕获）。
- **离线内存**：每负载一个进程级 RSS 样本。React 两种负载编译同一份产物，测得 47.9 和 48.1 MiB；其他负载为 5.0–5.5 MiB。

下一轮保持原定的两个独立对照：CFG 常量传播对比数值与 React 分派次数，静态属性键对比属性负载的分派、槽数和稳态时间。静态键本身不应以减少属性探测次数为目标。另将 React 的解码和启动准备作为单独的加载实验，不与编译器 pass 混合归因。
