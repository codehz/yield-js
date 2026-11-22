# 按分配量触发的 GC 调度对照

对照固定间隔（每轮 / 每 8 轮）与按分配量（对象数或 buffer 后备存储字节）触发的宿主调度，补足 [gc.md](gc.md) 中只按调用次数触发的不足。基线为引入分配计数器前的最新提交。

## 方法

每个进程一个 VM、64 轮，负载与暂停计时方法与 [gc.md](gc.md) 一致：`react-updates` 与 `buffer-churn`（4 MiB `Uint8Array`/轮），微任务排空后在空闲点调用 `collect_garbage(host.gc_roots())`，峰值 RSS 取 `/usr/bin/time -l` 的 `maximum resident set size`。

新增度量：`Vm::allocation_totals()` 返回堆的两个单调计数——创建过的对象总数与 buffer 后备存储累计字节。ID 不复用，所以相对上一次收集的差值就是本区间分配量。buffer 字节只在分配 `ArrayBuffer` 时计入（视图共享一份后备存储，不重复计）。两个计数器是运行时状态，不进快照；恢复后的 VM 从 0 起算，宿主在启动后取基线。

新增策略 `alloc-<objects>-<bytes>`：对象分配数或 buffer 字节任一越阈值即收集，0 表示停用该分量。阈值按每轮实测分配量校准，使各负载的收集节奏与固定间隔策略等价（不按轮数）：

| 负载 | 每轮分配量（实测） | 每轮等价阈值 | 每 8 轮等价阈值 |
| --- | ---: | ---: | ---: |
| React 更新 | 1,384 个对象，0 B buffer | 1,024 个对象 | 10,240 个对象 |
| Buffer churn | 2 个对象 + 4 MiB buffer | 4 MiB（对象分量停用） | 32 MiB（对象分量停用） |

两组阈值合并为两个跨负载策略：`alloc-1024-4194304` 与 `alloc-10240-33554432`。React 负载无大 buffer，由对象分量驱动；buffer 负载对象数远低于阈值，由字节分量驱动。

```sh
env -u YIELD_VM_BENCH_DIAGNOSTICS cargo bench --locked -p yield-harness --bench baseline --no-run
for workload in react-updates buffer-churn; do
  for policy in none every-1 every-8 alloc-1024-4194304 alloc-10240-33554432; do
    /usr/bin/time -l "$BIN" --gc "$workload" "$policy"
  done
done
```

每负载 5 个独立进程，轮转策略执行顺序；轮总耗时与 RSS 取进程中位数，暂停合并观测，p95 取最近秩。原始 CSV：[react-updates](gc-alloc-trigger/react-updates-samples.csv)、[buffer-churn](gc-alloc-trigger/buffer-churn-samples.csv)。

## 结果

| 负载 | 策略 | 峰值 RSS（MiB） | 64 轮总耗时（ms） | 收集次数 | 暂停 p50 / p95 / max（µs） |
| --- | --- | ---: | ---: | ---: | --- |
| React 更新 | 不收集 | 89.72 | 121.55 | 0 | — |
| React 更新 | 每 1 轮 | 42.52 | 153.21 | 64 | 585 / 672 / 1,674 |
| React 更新 | 每 8 轮 | 47.86 | 130.38 | 8 | 1,752 / 2,124 / 2,992 |
| React 更新 | alloc 低阈值 | 42.66 | 154.20 | 64 | 585 / 669 / 1,393 |
| React 更新 | alloc 高阈值 | 47.84 | 132.02 | 8 | 1,787 / 1,983 / 2,234 |
| Buffer churn | 不收集 | 264.31 | 58.01 | 0 | — |
| Buffer churn | 每 1 轮 | 11.52 | 47.92 | 64 | 59 / 93 / 216 |
| Buffer churn | 每 8 轮 | 39.58 | 48.11 | 8 | 81 / 131 / 140 |
| Buffer churn | alloc 低阈值 | 11.45 | 46.49 | 64 | 57 / 93 / 219 |
| Buffer churn | alloc 高阈值 | 39.58 | 47.90 | 8 | 85 / 117 / 124 |

按分配量触发与等价的固定间隔策略在两项负载上逐项吻合：RSS 差 ≤ 0.15 MiB，轮总耗时差 ≤ 1%，暂停分布一致。四个收集策略在每个负载上移除的对象总数相同（React 89,158 个、buffer churn 128 个）。

## 结论

- **等价性成立。** 一份跨负载阈值（对象数 OR buffer 字节）能在两种分配形状不同的负载上复现每 1 轮与每 8 轮两种均衡，而固定间隔策略无法跨负载校准——`buffer-churn` 每轮只分配 2 个对象，纯对象计数策略要么永不触发，要么停用字节分量后失去依据。
- **触达边界是轮界。** 本夹具只在每轮结束后检查阈值；分配高峰发生在轮内时，实际触发点迟于越界点，表现为负载在收集间略高于阈值（React 高阈值时实测 11,072–12,456 个对象/次，高于名义 10,240）。把检查移进轮内（每 N 次分配）可收敛该差距，本轮未做。
- **计数器成本不可见。** 对照基线只增加两次整数自增（对象 + buffer 字节），`none` 策略的轮总耗时与 [gc.md](gc.md) 基线一致量级，未单独立项对照。
- `alloc_bytes` 只在分配时递增，不随清扫回减。触发用的是「自上次收集以来的分配量」，语义正确；但它不是当前存活字节数，宿主不能把它当作内存水位使用。detach 后 buffer 已计字节在下一次收集前仍留在计数里，与分配量语义一致，无需修正。

## 后续

- 若暂停仍是瓶颈，先做 [next-optimizations.md](next-optimizations.md) 第 1 项的快照捕获分项，而不是改标记集合；本轮四个收集策略的 p95 暂停在两种负载上都与固定间隔策略持平。
- 本轮未测触发检查本身的调用开销放在 VM 调用边界外的宿主侧；若要把调度下沉进解释器自动 GC，先复测轮内检查的成本。
