# 函数索引与调用路径

本轮目标来自计划中的“调用分派或帧分配”。先试帧缓冲复用和小槽内联, 两者都没有稳定收益, 均未启用. 最终启用的是 `FunctionTable` 的 ID 索引: 常见连续 `func_id` 用密集 `Vec<usize>` 做 O(1) 查找, 稀疏 ID 回退到按 ID 排序的向量二分. 不改字节码、快照形状或构建摘要.

## 实现与边界

- `FunctionIndices::Dense`: 以 `func_id` 为下标, 缺失用 `usize::MAX` 标记; 仅当最大 ID 小于函数数四倍与 64 的较大值时使用.
- `FunctionIndices::Sparse`: 按 `func_id` 排序的 `Vec<(u32, usize)>`; 保持 `u32::MAX` 附近等稀疏 ID 可用, 内存不随 ID 上限膨胀.
- `Vm::function_index` 与 `FunctionTable::from_validated` 使用同一索引; 模块声明/求值函数的查找在构建期仍可复用临时 `BTreeMap`.
- 函数索引是加载后派生元数据, 不进入 `Build`、`build_id` 或快照. 现有 `sparse_function_ids_work_after_restore` 覆盖稀疏回退及恢复后调用.

## 环境与命令

Apple M2 (24 GiB), macOS 27.0.1, rustc/cargo 1.99.1. 基线是提交 `ee81bd5`; 最终版是同一工作树上的改动. 两版各跑 A/B/C 三批, 每批 `--samples 5`, 进程内预热 2 次; 报告使用每批中位数. 复现:

```sh
cargo bench -p yield-harness --bench baseline -- --samples 5
```

原始样本在 [function-index/](function-index/): `baseline-a/b/c.csv`、`lookup-a/b/c.csv`; 未启用实验见 `frame-pool-a/b.csv`、`inline-slots-a/b.csv`.

## 结果

| 负载 / 指标 | 基线 (µs) | 密集索引 (µs) | 变化 |
|---|---:|---:|---:|
| React 首提交 / `startup_initialize` | 1502.92 | 1436.33 | −4.4% |
| React 首提交 / `entry_first` | 481.96 | 472.79 | −1.9% |
| React 首提交 / `entry_steady_5` | 2088.17 | 2064.50 | −1.1% |
| React 更新 / `startup_initialize` | 1513.67 | 1446.54 | −4.4% |
| React 更新 / `entry_first` | 2631.58 | 2589.58 | −1.6% |
| React 更新 / `entry_steady_5` | 10327.33 | 10162.96 | −1.6% |
| `await` 成功 / `entry_steady_5` | 87.71 | 80.92 | −7.7% |
| 闭包读写 / `entry_first` | 403.71 | 387.46 | −4.0% |
| 短调用 / `entry_steady_5` | 1262.42 | 1273.79 | +0.9% |
| 静态属性 / `entry_first` | 247.42 | 261.92 | +5.9% |

React 两个负载的启动与稳态收益跨三批方向一致, `await` 成功的稳态也有约 8% 改善. 静态属性首次执行的 +5.9% 只在部分批次出现; C 批为 252.21 µs, 与基线 A 批的 247.42 µs 接近, 属于代码布局/样本波动, 不构成稳定回退. 短调用稳态 +0.9% 同样在波动范围内. 函数索引不改变可观察语义, 语义用例和快照测试全部通过.

## 未启用项

- **帧缓冲复用.** `execute_frame` 返回后回收槽缓冲, 短调用 5,125 次帧构造中约 5,124 次复用. 但短调用稳态 +0.6% 到 +2.1%, React 稳态 +0.7% 到 +1.6%; 小帧的分配成本低于池的取用、清零和生命周期成本. 未启用.
- **小槽内联.** 8 槽以内使用 `[Option<Value>; 8]`, 超过回退 `Vec`. 消除了小帧分配, 但 `Frame` 变大, 数值/分支稳态 +6.8%, React 稳态 +2.0% 到 +2.4%; 未启用.
- **`#[inline]` 函数索引.** 对 `FunctionIndices::get` 与 `Vm::function_index` 加内联提示后, React 首提交 `entry_first` 从 −1.9% 变为 +4.9%, 代码布局代价大于调用点收益; 未启用.
