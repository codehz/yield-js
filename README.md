# yield-js PoC

可恢复迷你 JavaScript VM 的第一个垂直切片。当前实现覆盖设计文档中最关键的闭环：

```text
严格模式 JS 子集
  -> Oxc AST
  -> 显式槽位字节码（构建 ID / 恢复表）
  -> async VM（Promise / 微任务 / 续体）
  -> 宿主 operation
  -> 快照 / 恢复
```

## 运行

```sh
cargo test --workspace
cargo run -p yield-poc
```

`yield-poc` 编译并执行设计文档中的双 `await` 例子，在两个挂起点分别快照、丢弃 VM、恢复并完成宿主 operation。输出会显示：

- 每次挂起保存的持久槽值：第一次 `[10]`，第二次 `[12]`；
- `inspect` / `seed` 在恢复后不会重跑；
- 最终结果 `15`。

## 目录

| 路径 | 内容 |
| --- | --- |
| `crates/vm` | 字节码契约、加载校验、对象表、Promise、微任务、async 续体、宿主协议、快照恢复 |
| `crates/compiler` | Oxc 0.159.0 解析、严格模式子集、CFG 活性分析、瞬时/持久槽分配 |
| `crates/poc` | 端到端测试与可运行示例 |

## 已实现

- 字节码版本、构建 ID 摘要、函数/导入/常量/恢复表结构校验。
- `SlotKind::{Transient, Persistent}`；`ResumeEntry` 保存 `fulfill_pc`、`reject_pc`、结果槽和升序 `saved_slots`。
- 编译期 CFG 活性分析：`saved_slots` 取成功/拒绝恢复入口 live-in 的并集，再扣除结果槽。
- 不跨挂起点的槽分配为瞬时槽；挂起时丢弃瞬时槽，恢复时只重建指定持久槽。
- 双挂起点示例实际生成 `resume 0 -> [0]`、`resume 1 -> [1]`，并保存对应值 `10`、`12`。
- VM 自有对象表和对象 ID；对象环、Promise 反应、续体与等待关系都按 ID 保存。
- async 函数、`await`、Promise 微任务恢复；已结算 Promise 也必须经微任务恢复。
- 宿主同步导入、可恢复 operation、operation 完成/拒绝、重复完成拒绝。
- 原生对象稳定键 `(type, id)` 包装；恢复前验证绑定，缺失即整体拒绝。
- 快照仅遍历根可达对象；从 pending operation、async 续体和脚本根出发。
- 恢复校验：格式/字节码版本、构建 ID、槽值数量、函数/恢复点、Promise/续体关联、缺失 operation、缺失 native 绑定。
- 快照要求微任务队列已排空。
- React/Solid 所需语法尚未实现；当前兼容基线不是首版完整验收。

## 当前子集

已支持的主要语法：

- 顶层 `function` 声明；`async` 函数；`await`。
- `var` / `let` / `const`（无 TDZ 精确保留）、块作用域、简单参数。
- 字面量、标识符、算术/比较/严格与宽松相等/位运算、短路、`??`、条件表达式、序列表达式。
- 赋值与算术复合赋值、`typeof` / `void`、普通函数调用、导入调用。
- `if`、`while`、`do...while`、`for`、`break`、`continue`、`return`、`throw`。

不支持的语法在编译期报告 `unsupported syntax: ...` 与源码行列。

## 已知未覆盖

- 闭包、函数表达式/箭头函数、对象/数组字面量与成员访问。
- `try/catch/finally`、`switch`、`for...in`、`for...of`、标签。
- `class`、模板字符串、generator、async generator、顶层 `await`。
- Proxy、Map/Set/WeakMap、完整内建对象与标准库。
- Promise thenable 同化、`Promise.then` 等公开方法、`queueMicrotask`。
- 运行期 tracing GC（目前有对象表与快照可达性遍历，没有回收器）。
- C ABI、Apple/Android 宿主适配器、React/Solid 验收。
