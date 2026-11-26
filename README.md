# yield-js PoC

可恢复迷你 JavaScript VM 的第一个垂直切片。当前处于早期开发状态：已打通「严格模式 JS 子集 → 字节码 → async VM → 静态宿主绑定 → 快照/恢复」的核心闭环，但仅覆盖设计文档的一个子集，语法与内建仍不完整。

```text
严格模式 JS 子集
  -> Oxc AST
  -> 显式槽位字节码
  -> async VM（Promise / 微任务 / async 续体）
  -> 静态宿主模块 / 类 / operation
  -> 快照 / 恢复
```

## 运行

```sh
npm ci --ignore-scripts --prefix fixtures/npm-react-app  # Blitz 首屏测试数据
cargo test --workspace
cargo run -p yield-harness
```

`yield-harness` 编译并执行双 `await` 例子：在两个挂起点分别快照、丢弃 VM、恢复并完成宿主 operation，最终打印结果。编译器通过同步解析器静态链接严格 ES 模块；宿主只访问入口模块的公开导出。VM 执行和恢复均不需要源文件或解析器。

`yield-blitz` 从 JSX、CSS 和锁定版本的 React npm 源码编译可交互示例。按上面的命令安装依赖后，运行 `cargo test -p yield-blitz` 做无窗口验收，或运行 `cargo run -p yield-blitz` 打开原生窗口，点击按钮增加计数。Rust 不会自动安装 npm 依赖。

## Rust 嵌入

`Vm` 在新建或恢复时按值接管 `Host`；后续执行不再传入宿主。无宿主模块和服务时用 `Host::default()`；用 `host!` 注册静态绑定或仅注册平台服务。

```rust
use yield_vm::{Host, RestoreOptions, Vm, VmOptions};

let mut vm = Vm::new(build.clone(), Host::default(), VmOptions::default())?;
vm.run_until_idle()?;
let snapshot = vm.snapshot(&[])?; // 没有需要额外保存的宿主持有值
let mut restored = Vm::restore(snapshot, build, Host::default(), RestoreOptions::default())?;
restored.start_recovered_operations()?;
restored.run_until_idle()?;
```

宿主通过 `host!` 以 `name: value` 注册内建平台服务，以 `Type => value` 注册应用服务。快照只保存 VM 状态；恢复时可提供不同 Rust 平台类型的宿主，只要满足运行需要。服务、根和恢复启动的契约见[宿主接口](docs/design/host-interface.md)与[快照设计](docs/design/snapshot.md)。

## 目录

| 路径 | 内容 |
| --- | --- |
| `crates/vm` | 字节码契约、加载校验、对象表/环境/闭包、Promise、微任务、async 续体、宿主协议、快照恢复 |
| `crates/compiler` | Oxc 解析、严格模式子集、词法绑定、异常降低、CFG 活性分析、槽分配 |
| `crates/harness` | 端到端测试、`tests/cases` 语义用例与可运行示例 |

## 状态文档

README 不记录实现细节；当前实现范围、内建支持、快照格式版本、限制与缺口均见状态文档（由代码推导）：

- [`docs/implemented.md`](docs/implemented.md) — 已实现内容清单。
- [`docs/gaps.md`](docs/gaps.md) — 未实现与缺口清单。

目标设计见 [`docs/design.md`](docs/design.md)。
