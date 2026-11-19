# yield-js PoC

可恢复迷你 JavaScript VM 的第一个垂直切片。当前实现覆盖设计文档中最关键的闭环：

```text
严格模式 JS 子集
  -> Oxc AST
  -> 显式槽位字节码（构建 ID / 恢复表 / 异常表）
  -> async VM（Promise / 微任务 / 续体 / 闭包环境）
  -> 宿主 operation
  -> 快照 / 恢复
```

## 运行

```sh
cargo test --workspace
cargo run -p yield-poc
```

`yield-poc` 编译并执行设计文档中的双 `await` 例子，在两个挂起点分别快照、丢弃 VM、恢复并完成宿主 operation。输出会显示：

- 未捕获的局部值在持久槽中，按 `saved_slots` 保存；闭包共享的绑定仍在调用环境，没有捕获时快照不保留该环境；
- `inspect` / `seed` 在恢复后不会重跑；
- 最终结果 `15`。

## 目录

| 路径 | 内容 |
| --- | --- |
| `crates/vm` | 字节码契约、加载校验、对象表/环境/闭包、Promise、微任务、async 续体、宿主协议、快照恢复 |
| `crates/compiler` | Oxc 0.159.0 解析、严格模式子集、词法绑定、异常降低、CFG 活性分析、槽分配 |
| `crates/poc` | 端到端测试、`tests/cases` 语义用例与可运行示例 |

## 已实现

- 字节码版本、构建 ID 摘要、函数/导入/常量/恢复表/异常表结构校验。
- `SlotKind::{Transient, Persistent}`；`ResumeEntry` 保存 `fulfill_pc`、`reject_pc`、结果槽和升序 `saved_slots`。
- 编译期 CFG 活性分析：每个恢复入口先扣除该入口的结果槽，再取并集作为 `saved_slots`；异常边参与活性。
- 不跨挂起点的槽分配为瞬时槽；挂起时丢弃瞬时槽，恢复时只重建指定持久槽。
- 未跨函数共享的绑定在帧槽中；闭包共享的绑定仍在堆环境，快照保留环境、闭包与对象身份。没有共享绑定且不创建闭包的调用不分配环境。
- 函数声明提升；函数表达式、箭头函数、函数值成员调用。
- 对象/数组字面量、计算属性键、成员读写、数组 `length` 与索引、复合成员赋值。
- `try/catch/finally`：异常表、catch 绑定、finally 正常/throw/return 完成状态、嵌套 finally、await 中的待完成状态。
- VM 自有对象表和对象 ID；对象环、Promise 反应、续体与等待关系都按 ID 保存。
- async 函数、`await`、Promise 微任务恢复；已结算 Promise 也必须经微任务恢复。
- 宿主同步导入、可恢复 operation、operation 完成/拒绝、重复完成拒绝。
- 原生对象稳定键 `(type, id)` 包装；恢复前验证绑定，缺失即整体拒绝。
- 快照仅遍历根可达对象；从 pending operation、async 续体、闭包环境和脚本根出发。
- 恢复校验：格式/字节码版本、构建 ID、槽值数量、函数/恢复点、异常表、Promise/续体关联、环境是否与函数布局一致、闭包引用、缺失 operation、缺失 native 绑定。
- 快照要求微任务队列已排空。

## 当前子集

已支持的主要语法：

- 顶层 `function` 声明；嵌套函数声明（提升）；函数表达式；箭头函数；`async` 函数；`await`。
- `var` / `let` / `const`（无 TDZ 精确保留）、块作用域、简单参数。
- 字面量、对象/数组字面量（Init 属性、静态/计算键、数组空洞按 `undefined`）、成员读写。
- 算术/比较/严格与宽松相等/位运算、短路、`??`、条件表达式、序列表达式。
- 赋值、算术复合赋值与逻辑赋值（`&&=`、`||=`、`??=`）、`typeof` / `void`、普通函数调用、函数值调用、导入调用。
- `if`、`while`、`do...while`、`for`、`break`、`continue`、`return`、`throw`、`try/catch/finally`。

不支持或尚未精确实现的语法在编译期报告 `unsupported syntax: ...` 与源码行列。

## 已知未覆盖

- 对象方法/getter/setter、展开属性/元素；对象/数组跨宿主边界（当前只在 VM 内使用）。
- `this`、类、模板字符串、generator、async generator、顶层 `await`。
- `switch`、`for...in`、`for...of`、标签；跨 finally 的 `break/continue`（编译期拒绝）。
- 每轮循环独立绑定、TDZ 精确报错、catch 参数解构。
- Proxy、Map/Set/WeakMap、完整内建对象与标准库。
- Promise thenable 同化、`Promise.then` 等公开方法、`queueMicrotask`。
- 运行期 tracing GC（目前有对象表与快照可达性遍历，没有回收器）。
- C ABI、Apple/Android 宿主适配器、React/Solid 验收。
