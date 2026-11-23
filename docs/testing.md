# 测试与用例契约

> 实现范围见 [`implemented.md`](implemented.md)，缺口见 [`gaps.md`](gaps.md)；内部契约与改动落点见 [`internals.md`](internals.md)。

全部测试经 `cargo test` 运行。四层结构：

1. `crates/compiler/tests/compile.rs` — 双 `await` 示例的字节码形状与不支持语法的位置。
2. `crates/harness/tests/e2e.rs` 与 `crates/harness/tests/e2e/` 模块 — 快照身份、加载器拒绝、跨 `await` 的 `finally`、可达性、生成器恢复表、GC 等。用 `matches!` 断言 `VmError`。
3. `crates/harness/tests/cases/` — 由 `build.rs` 生成测试的语义 `.js` 用例（见下）。
4. `crates/host-dts/src/lib.rs` 单元测试，以及 `crates/binding-macros/tests/host_dts_fixture.rs`：它构建一个一次性消费 crate 并运行 `cargo check`，证明“已声明但未实现”的导出在宏展开期失败。

没有 insta，没有覆盖率门槛，禁止 `#[ignore]` / `#[should_panic]`。VM 的行为经 `yield-harness` 测试，不要在 `vm` 或 `compiler` 源码内加行为测试模块。唯一例外是 `crates/vm/src/bytecode.rs` 与 `crates/vm/src/bytecode/instr.rs` 的字节码契约单元测试：产物编码字节、指令表里 `slots[...]` 的检查顺序与标签。它们不经宿主，也不该为测试放宽 `pub(super)` 的验证入口。

需要堆身份、槽表或拒绝恢复的断言写进 `e2e.rs`；可观察的 JS 行为写 `.js` 用例，不要新增 Rust harness。

手动破坏镜像以检查内部不变量时，为 `Vm::restore` 传入 `RestoreOptions { validation: SnapshotValidation::Full, ..Default::default() }`。可信镜像的日常往返，以及生产入口必须保留的构建、宿主兼容性检查，使用 `RestoreOptions::default()`（`Trusted`）。

## 用例契约（`yield_harness::run_case`）

- 文件头是前导空行与 `// @` 指令。整份文件（含头部）一起编译，因此诊断保留文件行号。
- 正文必须显式导出一个可调用绑定，公开名为 `run`；运行器总是启动 `"run"`。未导出的 `run` 报 `unknown export "run"`。
- `@args` 是 JSON 数组，只能作为第一条指令。
- `@pending ["wait:a"]` 断言当前 pending operation 集合；`@restore` 做 JSON 往返再用 `Vm::restore` 恢复，沿用测试时钟（通过 `RestoreOptions::test_clock` 注入）和默认的 `Trusted` 校验模式；`@fulfill ["wait:a", 2]`、`@reject ["wait:a", "boom"]` 交付完成。
- `@advance <u64>` 按十进制毫秒推进测试时钟，并在每个到期 timer 之后排空工作。负数、小数、`+1`、超出 `u64` 或超出 ECMAScript Date 范围的推进都带位置报错。
- 最后一条指令必须是 `@returns`、`@fulfilled`、`@rejected` 或 `@uncaught`。标量仅限 number、bool、string、`null`、`undefined`。`@uncaught` 接收数组，按顺序精确比对报告给宿主的错误列表。
- 用例从 `test` 显式导入宿主导出。`gc()` 同步回收且不排空微任务；`spawnDelayedTask` 返回可取消的宿主任务句柄，`cancelDelayedTask` 取消它，`sleep` 返回原生 Promise。VM 定时器用裸全局测试。
- 用例运行器通过显式测试时钟入口创建与恢复 VM。需要亚毫秒、部分投递或无唤醒服务的定时器断言放在 e2e；不等待真实时间。

```js
// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 2]
// @fulfilled 2

export async function run() {
  return await wait("a");
}
```

## 用例目录

新用例放进 `crates/harness/tests/cases/` 下匹配类别的子目录：`abort`、`arrays`、`async`、`bigint`、`binary`、`closures`、`coercion`、`collections`、`const`、`control`、`date`、`descriptors`、`destructuring`、`errors`、`events`、`fetch`、`finally`、`functions`、`gc`、`globals`、`host`、`json`、`loops`、`math`、`numbers`、`objects`、`optional_chain`、`performance`、`promise`、`prototypes`、`proxy`、`reflect`、`regexp`、`strings`、`switch`、`symbols`、`timers`、`url`。不要在 `tests/cases/` 顶层新增用例。

`build.rs` 递归遍历目录，文件名用 `snake_case.js`；stem 与类别名都必须匹配 `[a-z][a-z0-9_]*`；生成的测试名为 `case_<category>_<stem>`（顶层为 `case_<stem>`）。每个目录都登记 `cargo:rerun-if-changed`，目录为空或缺失会 panic。指令解析器自身的失败用例放在 `crates/harness/tests/cases.rs`，夹具源码在 `crates/harness/tests/failing_cases/`。

Fetch 的网络与切点测试放在 `crates/harness/tests/e2e/`：`fetch_http.rs` 是脚本化 loopback HTTP/1.1 服务器，按序记录每个请求；`fetch_network.rs` 用真实连接覆盖 CORS/预检、跨源重定向、307 不可重放体、`only-if-cached`、SRI、gzip、keepalive、abort 与上传额度，并用 `ControllableFetchService` 精确推进 `UploadCredit`/`Head`/`Data`/`Eof`/`Failed` 以及上传、等 head、读体和 EOF 各阶段的快照恢复。离线可观察行为仍写在 `cases/fetch/`，不新增 Rust harness。

## 宿主声明夹具

- 真实宿主：`crates/harness/test.d.ts`、`crates/harness/demo.d.ts`。
- e2e 夹具：`crates/harness/tests/dts/<modules>.d.ts`，模块名排序后用 `-` 连接；只链接 `TestHost` 的夹具经 `support::TEST_DECLARATIONS` 复用 `harness/test.d.ts`。
- 宏测试：`crates/binding-macros/tests/dts/`。`host_dts_fixture` 以 `cargo test` 设置的包根目录为起点，在运行时定位 workspace，避免共用 `target` 的不同检出复用带有旧路径的测试二进制时访问错误位置。
