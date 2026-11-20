# yield-js PoC

可恢复迷你 JavaScript VM 的第一个垂直切片。当前处于早期开发状态：已打通「严格模式 JS 子集 → 字节码 → async VM → 宿主 operation → 快照/恢复」的核心闭环，但仅覆盖设计文档的一个子集，语法与内建仍不完整。

```text
严格模式 JS 子集
  -> Oxc AST
  -> 显式槽位字节码
  -> async VM（Promise / 微任务 / async 续体）
  -> 宿主 operation
  -> 快照 / 恢复
```

## 运行

```sh
cargo test --workspace
cargo run -p yield-poc
```

`yield-poc` 编译并执行设计文档中的双 `await` 例子：在两个挂起点分别快照、丢弃 VM、恢复并完成宿主 operation，最终打印结果。宿主只能按公开名读取或调用显式导出的绑定（`export async function run`）；输入是离线打包后的单个严格 ES 模块，VM 不加载或链接其他模块。

## 目录

| 路径 | 内容 |
| --- | --- |
| `crates/vm` | 字节码契约、加载校验、对象表/环境/闭包、Promise、微任务、async 续体、宿主协议、快照恢复 |
| `crates/compiler` | Oxc 解析、严格模式子集、词法绑定、异常降低、CFG 活性分析、槽分配 |
| `crates/poc` | 端到端测试、`tests/cases` 语义用例与可运行示例 |

## 状态文档

README 不记录实现细节；当前实现范围、内建支持、快照格式版本、限制与缺口均见状态文档（由代码推导）：

- [`docs/implemented.md`](docs/implemented.md) — 已实现内容清单。
- [`docs/gaps.md`](docs/gaps.md) — 未实现与缺口清单。

目标设计见 [`docs/design.md`](docs/design.md)。
