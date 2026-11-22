# GC 暂停与进程峰值内存

基于 `368d7d1`，只增加基准，不改变 VM 或 GC 策略。Apple M2 / macOS 27.0.1，未启用诊断计数的 release 构建。

## 方法

- 每个进程只启动一个 VM，执行 64 轮。`react-updates` 首轮挂载，随后每轮验证 8 次 React 更新；`buffer-churn` 每轮分配 4 MiB `Uint8Array`，每隔 4 KiB 写入一次以触及物理页，读取最后一个写入点校验结果，不保留数组。
- 三种宿主策略：`none` 不收集，`every-1` 每轮收集，`every-8` 每 8 轮收集。在微任务排空后调用 `collect_garbage(host.gc_roots())`。暂停计整次调用，包括标记、清扫与缓存失效；循环总耗时含暂停，不含编译或 VM 启动。
- 每个负载与策略组合运行 5 个独立进程，轮转三种策略的执行顺序。峰值 RSS 取 macOS `/usr/bin/time -l` 的 `maximum resident set size`，包含编译器、VM、分配器和进程其他内存；不是 VM 堆独占指标。总耗时与 RSS 取进程中位数；暂停合并各进程观测，p95 取最近秩。没有舍弃第一次收集。

```sh
env -u YIELD_VM_BENCH_DIAGNOSTICS cargo bench --locked -p yield-harness --bench baseline --no-run
# BIN 取上一命令打印的基准可执行文件路径。
for workload in react-updates buffer-churn; do
  for policy in none every-1 every-8; do
    /usr/bin/time -l "$BIN" --gc "$workload" "$policy"
  done
done
```

[原始 CSV](gc/samples.csv) 保存逐次暂停、回收对象数、每进程循环总耗时与峰值 RSS。

## 结果

| 负载 | 策略 | 峰值 RSS（MiB） | 64 轮总耗时（ms） | 暂停样本 | 暂停 p50 / p95 / max（µs） |
| --- | --- | ---: | ---: | ---: | ---: |
| React 更新 | 不收集 | 90.53 | 120.64 | 0 | — |
| React 更新 | 每轮 | 42.66 | 152.12 | 320 | 582 / 663 / 1,733 |
| React 更新 | 每 8 轮 | 47.86 | 130.99 | 40 | 1,792 / 2,111 / 3,562 |
| Buffer churn | 不收集 | 264.42 | 56.07 | 0 | — |
| Buffer churn | 每轮 | 11.55 | 45.66 | 320 | 66 / 88 / 106 |
| Buffer churn | 每 8 轮 | 39.64 | 45.96 | 40 | 88 / 129 / 164 |

两种收集策略每进程移除的对象总数相同：React 89,158 个，buffer churn 128 个。对象数不能代表回收字节数；buffer 负载尤其明显。

- **React：** 每 8 轮比每轮多用约 5 MiB 峰值 RSS，但循环少约 21 ms；代价是 p95 暂停从 0.66 ms 增至 2.11 ms。做低延迟调度时不能只看循环总耗时。
- **Buffer churn：** 每轮收集比每 8 轮少约 28 MiB 峰值 RSS，两者循环耗时接近。不收集时每轮保留旧 buffer；该结果不代表 GC 普遍提高吞吐。

本轮对照的是调度策略，未对比 `368d7d1` 前后的 GC，也未拆分标记和清扫。若需要跨负载共用触发策略，下一组对照应同时考虑分配对象数和后备存储字节数，而不只按调用次数触发。
