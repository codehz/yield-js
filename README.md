# yield-js PoC

可恢复迷你 JavaScript VM 的第一个垂直切片。当前处于早期开发状态：已打通「严格模式 JS 子集 → 字节码 → async VM → 宿主 operation → 快照/恢复」的核心闭环，但仅覆盖设计文档的一个子集，语法与内建仍不完整。

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

宿主只能按公开名读取或调用显式导出的活绑定。示例用 `export async function run` 公开入口；仅声明 `function run` 不会公开它。输入是离线打包后的单个严格 ES 模块，VM 不加载或链接其他模块。

`new Proxy(target, handler)` 支持普通对象和数组上的六种属性 trap，可用于成员访问、`Reflect`、对象复制及枚举。快照格式为 **28**，保存代理、目标、handler 及其闭包的身份，不执行 trap；旧格式不迁移。范围和限制见下方状态文档。

## 目录

| 路径 | 内容 |
| --- | --- |
| `crates/vm` | 字节码契约、加载校验、对象表/环境/闭包、Promise、微任务、async 续体、宿主协议、快照恢复 |
| `crates/compiler` | Oxc 0.159.0 解析、严格模式子集、词法绑定、异常降低、CFG 活性分析、槽分配 |
| `crates/poc` | 端到端测试、`tests/cases` 语义用例与可运行示例 |

## 状态文档

当前实现范围与未覆盖清单均由代码推导：

- [`docs/implemented.md`](docs/implemented.md) — 已实现内容清单（语法、内建、快照/恢复能力）。
- [`docs/gaps.md`](docs/gaps.md) — 未实现与缺口清单（编译期 `unsupported syntax: ...` 拒绝项、语义不完整点、结构性缺口）。

目标设计见 [`docs/design.md`](docs/design.md)。
