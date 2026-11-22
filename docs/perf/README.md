# 性能文档索引

`docs/perf/` 记录性能对照的方法、结论与后续方向。原始 CSV 数据留在对应子目录；过时轮次的原始数据随报告一并删除，负结果（未启用项）的数据保留。

| 文件 | 内容 |
|---|---|
|`performance-baseline.md`|基准方法：工作负载、计时阶段、诊断计数、峰值 RSS 采集与首轮基线数据。|
|`optimization-history.md`|已完成的各轮优化对照（结果、边界、未启用项）的合并记录。|
|`bytecode-loading.md`|加载与恢复路径的现状数据：二进制产物、`VerifiedBuild` 复用、二进制快照、恢复分项与可信恢复。|
|`explicit-cfg.md`|显式 CFG 的架构评估、实施结果与编译成本对照。|
|`bytecode-optimization.md`|离线字节码优化计划与正确性/交付门槛。|
|`next-optimizations.md`|下一轮优化方向（快照捕获、GC 调度、执行吞吐、离线编译）。|
|`gc.md`|GC 调度策略与进程峰值内存对照。|

## 通用方法约定

- 计时用未启用 `YIELD_VM_BENCH_DIAGNOSTICS` 的 release 构建；诊断计数单独编译运行，不对诊断构建计时。
- 每批独立进程预热 2 次，保留 5–15 个样本，报告中位数；跨批方向一致才视为稳定收益，小样本波动不据此宣称提速或回退。
- 对照在独立 worktree / Cargo target 中构建，共享同一 `Cargo.lock` 与夹具，交错或逆序运行抵消布局与顺序偏差。
- 指令形状变化提升 `BYTECODE_VERSION`；只有快照记录形状变化才提升 `SNAPSHOT_FORMAT_VERSION`，产物容器变化提升 `BUILD_FORMAT_VERSION`。
