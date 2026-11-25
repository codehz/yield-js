# 内部契约与改动指引

> 本文记录**现状**契约与索引：crate 流水线、公共入口、挂起/恢复与快照不变量、改动落点、易错点、代码约定细节。目标设计见 [`design.md`](design.md)，当前覆盖面见 [`implemented.md`](implemented.md)，缺口见 [`gaps.md`](gaps.md)，宿主绑定见 [`design/host-bindings.md`](design/host-bindings.md)，测试契约见 [`testing.md`](testing.md)。

## 1. 架构与数据流

crate 依赖单向：

```text
yield-harness  →  yield-compiler  →  yield-vm
                       ↓
                  yield-host-dts   ←  yield-binding-macros  (expansion only)
```

`yield-vm` 持有字节码契约，永不导入编译器。自定义源码和图转换可通过 `ParsedModule` 直接修改 workspace 固定版本的 Oxc `Program`，也可替换源码并重新解析。转换诊断可引用当前源码中的 Oxc `Span`；无源码位置的生成节点使用 `oxc::span::SPAN`。CFG 保持私有，Oxc 类型不进入 VM、字节码或宿主绑定。`yield-host-dts` 对外只暴露普通名字表与字节跨度。

```text
源码根与同步解析器（严格 ES 模块）
  → frontend     解析可达模块，按模块应用源码转换并发现依赖
  → graph        按注册顺序应用图转换，每次重新校验依赖并链接 import/export
  → lower        绑定、异常、await  →  LoweredFunction
  → bindings     确定哪些 id 跨函数共享
  → cfg          消费线性发射结果，建立带恢复边和异常边的基本块
  → promote      不共享的绑定变为逻辑寄存器；未逃逸循环绑定退出每轮环境
  → propagate    CFG 跨块传播字面量和复制，折叠纯运算和分支，选择右侧常量指令
  → cfg cleanup  校验 CFG，清理普通跳转，不改变源块顺序与恢复入口
  → analyze      按前驱重查 CFG live-in（含恢复边与异常边），移除无副作用的死写入
  → allocate     按原块序布局，流式记录瞬时槽区间并分配槽；保存集合取 CFG 块入口活性
  → Build        版本化字节码；build_id = 执行相关字段的摘要
  → build stages 按注册顺序转换 Build，每次重新计算 build_id 并校验
  → Vm           校验、运行、宿主操作、快照 / 恢复
```

公共入口：

- `Compiler::compile(Project) -> Result<Build, CompileError>` 链接显式根；`Compiler::from_dts(source)` 从宿主的 `.d.ts` 读出独立的宿主导入清单。默认生成 Error 栈位置；`stack_traces = false` 全部禁用。`ModuleResolver::resolve` 返回 `ResolvedModule`，同时给出规范模块 ID 与该定义模块的框架标记；`NodeResolver` 把 `node_modules` 中定义的模块标为框架，覆盖包 `exports` 目标、嵌套依赖和包内相对导入，其他解析器默认不标记。标记只决定是否生成栈位置，不改变解析、链接或运行语义；隐藏仅影响本模块，框架回调进入其他模块时后者仍可见。
- `Vm::new(build, host, VmOptions::default())` 按值接管不可克隆的 `Host`，校验构建与导入，实例化全部源码模块，求值可选 preload 子图并安装其活绑定 getter，再求值 entry 并排空初始化微任务。`Vm` 不带宿主类型参数；`Host::default()` 提供空宿主。
- `VmOptions::default()` 不设置基准 URL 或测试时钟。用 `VmOptions { base_url: Some("https://example.com/"), ..Default::default() }` 可在任何模块求值之前设置默认基准。构造和恢复的选项都可通过 `test_clock` 注入 `Arc<dyn TestClock>`，默认使用正常时钟。
- `Vm::get_export("name")` 读取入口链接导出的当前值（含再导出）；名字不唯一或不存在即错误。
- `vm.start("run", args)` 读取公开绑定并以 `undefined` 为接收者调用；`args` 是 `JsValue` 常量。随后调用 `vm.run_until_idle()`；所有执行入口都使用已接管的宿主，不再接收宿主参数。
- `vm.snapshot(roots)` 仅在微任务队列为空时可用；显式根只扩充捕获时的可达图，不保存为独立的宿主根清单，也不从宿主服务收集。
- `Vm::collect_garbage(roots)` 显式清扫两次调用之间不可达的堆状态，同样要求微任务队列为空；`roots` 是宿主仍需要的同 VM `Value` 切片。返回移除的堆对象数，并保留存活 ID。
- `Vm::restore(snapshot, same_build, host, RestoreOptions::default())` 接管新宿主，不重新求值模块。恢复的基准 URL（包括没有基准的情况）只来自快照；`RestoreOptions` 没有 `base_url` 字段，不能覆盖存档的基准。活动的 `URL`、`URLSearchParams` 及迭代器由普通堆强边追踪。
- `RestoreOptions::default()` 使用 `SnapshotValidation::Trusted`，只接收由 VM 捕获、未经篡改的有效镜像，不重查堆图。开发、测试和诊断可传入 `RestoreOptions { validation: SnapshotValidation::Full, ..Default::default() }` 执行完整不变量审计。两种模式都要求调用方保证来源和完整性并限制读入大小；完整审计并不使不可信输入变得安全。
- `Vm::new` 和 `Vm::restore` 都直接接受 `Build` 或 `VerifiedBuild`。传入 `Build` 时当次完整校验；`VerifiedBuild::new(build)` 消费并完整校验 `Build`，持有不可变构建和宿主无关的派生表，可在线程内克隆复用。两种输入都为每个 VM 独立链接宿主、持有堆和缓存；恢复仍要求精确匹配构建身份与绑定指纹。
- `CallCx::promise(op)` 以原生 Promise 为目标启动外部工作。链接的后台任务走同一注册路径，各自拥有可取消的数字句柄。通过 `Vm::complete_operation(token, completion)` 与当前 VM 代际 token 交付。注册在 drop 时释放平台资源。恢复后先发布 VM，再调用 `Vm::start_recovered_operations()`。
- `host!` 构造 `Result<Host, String>`；服务值装进宏生成的类型化结构体，整体擦除。链接按确切 Rust 类型将每个 `service_scope!` 的固定槽解析到该宿主的字段；回调从 `&Services` 绑定 scope 后直接投影字段。字段偏移与同一 `Bindings` 所拥有的存储配对，不得跨宿主使用。嵌入方在冷路径用 `vm.bind_service::<T>()` 解析并借用。scope、布局和服务实例不进入绑定指纹或快照。
- 内部的 `VmCore` 持有执行状态；公开的 `Vm` 将 core 与 `Host` 绑定，调用内部入口时借用宿主。销毁 VM 时先释放 core 及其活动登记，再释放宿主资源。快照从 core 捕获持久记录，捕获时可借用服务执行 codec 或 checkpoint，但不序列化运行时宿主。

## 2. 挂起、恢复与快照不变量（不得弱化）

- 操作数是显式槽。`Instr::Await` / `Instr::Yield` 不持久化任何隐式状态。
- `ResumeEntry` 是 `{ fulfill_pc, fulfill_dst, reject_pc, reject_dst, saved_slots }`。续体按 `resume_id` 索引，不按运行期 PC。
- `saved_slots` 是两个恢复入口各自 live-in 先减去该入口结果寄存器后的并集。结果槽在恢复时写入，不保存。持久槽不在不同活值之间复用。瞬时槽在挂起时丢弃。
- 瞬时槽按逻辑寄存器的保守线性区间复用：区间覆盖 CFG live-in、指令定义（包括死写入）、调用入口的参数写入，以及异常和恢复入口的注入写入。区间包含端点，同一条指令同时读写的不同逻辑寄存器不共享槽。复用只重映射操作数与表项，不插入清槽指令。
- 生成器经 `GeneratorEntry { next_pc, next_dst, throw_pc, throw_dst, return_pc, saved_slots }` 恢复，由 `Instr::Yield::generator` 索引。先分别减去每个入口自己的注入寄存器，再取三者 live-in 的并集；先取并集再减会丢掉另一入口需要的值。`return_pc` 没有结果槽：它以 `Instr::LoadGeneratorReturn` 开始，因此待完成的 payload 位于重建帧中，绝不进入 VM 全局状态。异步生成器自身的 `await` 使用普通 `ResumeEntry` 表和 `ContinuationTarget::Generator` 续体；两张表独立校验。
- `Instr::GeneratorStart` 在形参初始化后结束生成器调用：把 `Function::generator_start.saved_slots` 与本次调用环境捕获进新生成器对象并返回，正文不执行。只有启动捕获与三个 yield 入口由外部注入；普通控制流不得进入 `throw_pc`、`return_pc` 或 `body_pc`。
- 生成器对象是普通对象加一份暂停帧（`func_id`、状态、环境、按入口顺序排列的值）。异步生成器的记录另有 FIFO 请求队列；同步生成器的记录不含队列。`Executor` 状态从不进快照：捕获时写记录，恢复时拒绝。同步生成器重入恢复抛脚本 `TypeError`；异步生成器则改为追加请求。生成器只经 `Symbol.iterator` 消费，异步生成器只经 `Symbol.asyncIterator` 消费；不要在 `for...of` 中特判两者。
- 异步生成器调用不分配结果 Promise：`GeneratorStart` 返回生成器对象。每次 `next` / `throw` / `return` 创建请求 Promise、入队，并一次一步驱动队列。队首是进行中的请求；`yield` 以 `done: false` 兑现它，正文返回或落底以 `done: true` 兑现，未捕获脚本异常使生成器完成并拒绝它。正文中的 `await` 记录唯一续体、把生成器标为 `Awaiting`、保留队首请求，并恢复同一正文——绝不新建请求。已完成的生成器以 `{value: undefined, done: true}` 结算 `next`，拒绝 `throw`，并通过持久化的 `ReactionJob::GeneratorClose` 同化 `return` 参数后报告 `done: true`；后续请求永不越过队首。
- `undefined` 是真实值。帧槽中的 `None` 表示未初始化，而不是 `undefined`。
- 帧的瞬时槽和持久槽共用一块连续存储，但索引和边界各自独立。活动帧的两段槽都参与 GC；挂起时仍只按恢复表保存指定的持久槽。
- 被其他函数读写的绑定、以及导出绑定，留在堆环境中。其余绑定是帧槽。既不共享绑定也不创建闭包的调用不分配环境。
- 环境绑定访问有两层直接化。模块自身绑定改为 `GetModuleBindingDirect` / `SetModuleBindingDirect`，直接寻址模块环境而不走闭包链；这条路径不检查链接读取的 TDZ，嵌套闭包仍按普通读取看到未初始化的 `undefined`。其余绑定在提升后、函数及其所有者都没有 `SaveEnv` / `NewIterationEnv` / `RestoreEnv` 且闭包创建深度唯一时，改为带固定 `depth` 的 `GetBindingDirect` / `SetBindingDirect`；`depth` 从当前环境起算，模块环境为根，函数环境每层加一。带每轮环境的函数、类作用域闭包的外层绑定和创建深度不一致的函数保留 `GetBinding` / `SetBinding` 回退。两种 direct 写法都计入 `Function::needs_environment`（模块直接访问除外），槽分配与快照仍只知道普通环境/槽。
- 循环头 `let` / `const` 只有被嵌套函数读写时才保留在每轮环境；未捕获的绑定提升为帧槽，循环各轮复用同一槽。循环体内若有被嵌套函数读写的块级声明，保留空绑定表的每轮环境以提供新单元格；否则删除整组 `SaveEnv` / `NewIterationEnv` / `RestoreEnv`。
- `finally` 是显式完成状态（种类 + payload），不是原生异常栈。`finally` 内的嵌套 `await` 必须保留该待完成状态。
- 准备成功后，`Await` 总是经微任务队列恢复，即使是已 settled 的 promise。抛错的 `constructor` getter 直接转入脚本异常处理器，不挂起。
- 标签模板对象在 VM 启动时按 `Build::templates` 的顺序全部创建，是 GC 与快照根；快照按位置保存对象 ID。恢复要求数量一致、ID 唯一，且每个对象是冻结数组、元素与构建记录一致、`raw` 属性为固定描述符并指向同样冻结的 raw 数组。`Instr::GetTemplate` 只读取该表，不分配。
- 快照根包括带索引的源码模块环境与命名空间、内建、类原型、宿主模块状态槽、pending operation、Promise 拒绝候选及待交付的拒绝任务，以及调用方提供的脚本根。遍历可达闭包环境、宿主实例槽与异步续体。成功的微任务检查点只入队；拒绝事件在普通任务中派发，恢复本身不派发。原生指针、宿主回调与已入队的微任务不进快照。
- 活动事件路径仅存在于同步派发栈，由活动根保护，不进入快照。事件保留上次目标，监听器按登记 ID 保存。取消信号的 source/dependent 与监听器取消步骤中的目标是弱引用；GC 和快照捕获裁剪失活边，并仅在有活源且存在取消步骤或 `abort` 监听器时保活未取消的 dependent。
- VM 定时器共享一次性唤醒 operation。待唤醒时保存 VM 相对截止时间与调度序号；到期者撤销登记并按（截止时间、序号）入普通任务 FIFO。脚本任务只引用 ID 和序号，记录强引用回调和参数。普通任务可跨快照；已排队的一次性任务恢复不需要唤醒服务，有待登记的 timer 或会续订的 interval 则需要。
- `console` 的计数、计时起点和分组标题与 VM 命名空间根一同恢复。预览不含 JS 引用；计时起点与 snapshot.elapsed 共享相对时钟域。快照不携带 Printer 服务或输出历史。
- SubtleCrypto worker 只持有有界队列中的拥有型原语消息，不持有 JS 值；VM 侧待交付记录保活 Promise。`run_until_idle` 在微任务检查点后非阻塞地交付一个完成结果。快照拒绝任何待交付的原语操作；可导出 `CryptoKey` 的原始字节和元数据随普通堆身份保存，不可导出密钥没有可表示的记录。
- `fetch` 的脚本对象和获取状态都由 VM 持有：`Request`/`Response` 各有一条字节流 body，`BodyRead.active` 强引用当前读 promise（不借用 `stream_dependency` 指向 promise），`FetchState` 保存逐事务阶段、已接受但未运行的事件、上传额度和取消边。适配器只持有 operation token 与自己的连接；登记、后台队列和 socket 不进入快照。`deliver_fetch_event` 只核对代际、顺序与额度后入普通任务队列，不执行 JS：额度可累加，`Data` 只能消耗已授予的下载额度，最终 head 至多一次，`Eof`/`Failed` 终结唯一。恢复先按记录顺序消费镜像内事件，再对仍依赖外部传输的事务排一个本地 `AbortError` 中断任务：未暴露的响应拒绝原 Promise，已暴露的响应只让未完成的体出错；不重发、不启动新传输，旧代令牌的迟到事件被忽略。
- 恢复是精确版本匹配：`SNAPSHOT_FORMAT_VERSION`、`BYTECODE_VERSION`、`build_id` 与绑定指纹都必须一致。缺少声明、槽/payload 不匹配或缺少 operation 会拒绝整个恢复，VM 不得启动。不做快照迁移。
- `Snapshot::to_bytes` / `from_bytes` 使用 `YIELDSNP`、小端 `SNAPSHOT_BINARY_VERSION` 和小端快照记录版本，后接 bincode 小端变长整数记录。解码拒绝不匹配的版本和尾随数据，但不证明来源、完整性或堆图有效性。二进制容器或编码选项变更只提升 `SNAPSHOT_BINARY_VERSION`；JSON 快照格式保持独立。
- `ArrayBuffer` 的二进制编码批量写入后备字节；在当前 bincode 配置下，`serialize_bytes` 与 `Vec<u8>` 序列具有相同的长度前缀和原始字节，解码仍读 `Option<Vec<u8>>`。JSON 仍是字节数组（已分离时为 `null`）。

宿主绑定函数是同步的，不得重入 VM。声明不可变且共享；VM 拥有原型、实例、槽、payload、模块状态与 operation 记录。`JsValue` 句柄只在产生它的 VM 代际与调用内有效；保留值必须依赖声明的槽或模块状态槽。

## 3. 改动位置

| 改动 | 落点 |
| --- | --- |
| 语法子集 | `compiler/src/lower`（worklist，不是递归借用）、`frontend.rs`；用 `unsupported()` 拒绝 |
| 活性 / 哪些槽跨 await 或 yield 存活 | `analyze.rs`、`allocate.rs` |
| 绑定逃逸 / 循环头环境省略 / 直接环境访问 | `bindings.rs`、`promote.rs` 与 lowerer 的 `IterationEnvGroup`、`direct_bindings` / `function_depths` |
| 值传播 / 普通跳转 / 死写入 | `propagate.rs` 按 CFG 边求交，折叠已知原始值的严格比较和少量 Number 运算；`cfg/cleanup.rs` 清理普通跳转，`cfg/verify.rs` 校验入口与边；`analyze.rs` 删除无用的 `Move`、`Const`、`Undefined` 并重算活性；`cfg/layout.rs` 生成跳转 PC 和挂起表，并从块内活性流式累积槽区间，不保留逐 PC 活性表 |
| 新 opcode | `vm/src/bytecode/instr.rs`（指令表）、`vm/src/exec.rs`（step 按语义族分派；分支体在 `vm/src/exec/{value,branch,env,call,class,object,iterator}.rs`，值操作还需 `ops.rs`）、`compiler/src/instr.rs`、lowerer |
| 标签模板 | `compiler/src/lower/expr.rs`（引用与调用顺序）、`compiler/src/session.rs`（`TemplateSite` 表）；对象创建与快照在 `vm/src/init.rs`、`vm/src/gc.rs`、`vm/src/snapshot/capture/` |
| 新堆种类 | `vm/src/heap/object.rs` 的 `write_edges`（`ReactionJob` 的边同样在此）；GC 与快照捕获都经 `Heap::write_edges` 遍历，见 `vm/src/gc.rs`、`vm/src/snapshot/capture/` |
| 普通属性表 / 写侧站点缓存 | `vm/src/heap/types.rs` 的小表排序向量与大表 `BTreeMap`、`vm/src/heap/store.rs` 的 `plain_data_slot` / `set_plain_slot`、`vm/src/exec.rs` 的 `write_member` 预热与布局守卫（`SetMember` 指令分支在 `vm/src/exec/object.rs`） |
| 函数 ID 索引 / 调用分派 | `vm/src/vm.rs` 的 `FunctionTable` / `FunctionIndices`；连续 ID 用密集表，稀疏 ID 用排序向量二分。`vm/src/builtin/call.rs` 的 `Dispatch::Script` 携带 `callee` / `home` / `class`，避免 `invoke` 重复查堆 |
| 快照或恢复校验 | `vm/src/snapshot/`（`record.rs` 形状、`capture.rs` 入口与 `capture/` 下按校验、堆边、装载、续体、微任务分文件的恢复逻辑）；在 `harness/tests/e2e/` 补拒绝用例 |
| 宿主协议 | `vm/src/host/`（VM 接管的 `Host`、诊断与根访问服务、`Completion` 及平台服务契约）、`vm/src/binding/`（`schema.rs` 声明、`call.rs` 的 `CallCx`）；嵌入示例是 `harness/src/lib.rs`、`harness/src/main.rs` |
| 宿主声明语法 | `host-dts/src/lib.rs`（清单与拒绝）、`compiler/src/lib.rs`（`from_dts`）、`binding-macros/src/registry.rs`（`dts`、常量校验）、`binding-macros/src/typed.rs`（`ModuleExports`） |
| Fetch 对象与获取算法 | `vm/src/builtin/fetch/`（`blob`/`headers`/`form_data`/`body`/`request`/`response`/`controller`/`transaction`）；对象记录在 `vm/src/heap/types.rs` 与 `check_edges.rs`，控制器只在 VM 线程推进，适配器见 `vm/src/fetch.rs` |
| 全局别名 / navigator | `vm/src/builtin/globals.rs`（`BUILTINS` 只能追加，旧索引保持不变）、`vm/src/builtin/navigator.rs`（宿主 `user_agent`、编译目标平台、只读属性）、`vm/src/host/mod.rs` |
| 可观察的 JS 行为 | 一个 `.js` 用例，不是新的 Rust harness |
| ArrayBuffer / DataView / TypedArray / TextEncoder / TextDecoder 成员 | `vm/src/builtin/binary/`（`buffer.rs`、`view.rs`、`typed.rs`、`element.rs`、`codec.rs`、`text.rs`）；异形索引查找在 `vm/src/heap/binary.rs` |

## 4. 易错点

- 每个源码模块有各自同步的声明函数与求值函数。提升函数在正文执行前就闭包于模块环境。只有入口的链接导出表对宿主公开。快照保存模块根，恢复时绝不重跑初始化。嵌套函数是 Oxc 节点 worklist 加捕获的作用域链。
- 分配物理槽之前，每个 `Slot` 都只是塞进 `SlotKind::Transient` 的逻辑寄存器。优化不改写 PC；最终布局才把块边编码成跳转、恢复表和异常表，保留生成器独占入口，不登记没有存活可抛指令的保护区。槽结果死掉不代表指令可删：属性访问、JS 转换和调用可能执行脚本或抛异常。
- `compiler/src/instr.rs::may_throw` 必须标出任何可能投递 JavaScript 异常的 opcode，否则异常边不会进入活性分析与 promote。`VmError` 不是 JavaScript 异常。
- VM 持有只读的 `VerifiedBuild`，底层为 `Rc`；VM、宿主与服务注册表均按线程持有，不作为跨线程共享对象。每次进入 `execute_frame` 只克隆一次构建句柄，循环直接借用指令和变长操作数；不改变字节码、构建摘要或快照格式。
- 调用实参从调用者帧槽借用（`runtime::Arguments`）：`CallArgs::Slots` 是 `Arc<[Slot]>`，执行时直接借用槽表，序列化形状仍是数组；`Arguments::Slots` 只在被调函数形参绑定前读取，并依赖调用者帧在整个嵌套调用期间留在 `ActiveRoots` 链上。宿主、内建（intrinsic）与 bound 函数边界仍会按需物化为 `Vec`。
- Error 位置随指令穿过 CFG，最终按 PC 保存位置变化；`(0, 0)` 终止有来源区段，空表隐藏函数。位置表及定义模块索引参与构建摘要和校验，只在分配 Error 时沿 `ActiveRoots` 读取活动帧，使用预增 PC 减一查询。栈文本保留为普通字符串属性，不改变快照记录。
- `Function::needs_environment` 由形参与代码（`GetBinding` 之类）推导。`VerifiedBuild` 在校验后派生函数 ID 索引、调用环境需求和模块初始化标记；这些元数据不进入字节码、构建摘要或快照。任一初始化函数需要环境时，整个模块使用一份环境。
- 构建常量与宿主数字可能是非有限值。构建 JSON 与快照值使用同一套特殊数字标记；有限数字仍是 JSON number。
- ID 是带溢出检查的计数器（`ObjectId`、`ContinuationId`）。快照 JSON 顺序遵循 `BTreeMap`。
- `Instr::Binary` / `Instr::BinaryConst` 在两个操作数都是 `Number` 时直接执行 f64 算术、比较、相等和 ToInt32/ToUint32 位运算；任一操作数不是 `Number` 时回退 `ops::binary_op`，保持 BigInt、Symbol、字符串、对象和混合类型的转换与抛错顺序。编译器把右侧字面量（包括 CFG 证明的值）放入 `BinaryConst` 的常量池索引，不交换操作数；快速路径不做对象转换，也不缓存结果。
- 条件分支的 `BranchCompareFalse` / `BranchCompareTrue` 及常量变体直接求值比较并按结果跳转，不创建布尔结果槽；双 Number 比较直接返回条件，其他类型复用 `binary_value`。`if` / `while` / `do…while` / `for` 条件、条件表达式和 `switch` 的 `===` 使用该形式；`&&` 连接假分支，`||` 用真分支跳过右侧，`!` 使用 `BranchTrue`。分支目标、槽和常量索引在加载时校验，异常转换仍由当前异常表覆盖。
- `Instr::Unary` 对 `Number` 的 `Neg`、`Pos`、`BitNot`、`ToNumeric`、`Increment`、`Decrement` 直接计算，不进入 `ops::unary_op`；其余类型仍先执行对象转换和 Symbol 检查。`BranchBinary` / `BranchBinaryConst` / `BranchUnary` 是同一运算的无结果槽条件形式，供 CFG 优化后的单用途窥孔使用。
- `Math` 和 `Number` 的内建入口在实参已是 `Number` 时跳过数值转换；`Number` 静态 `is*` 方法不构造 coercion roots，`Math` 的 `max` / `min` / `hypot` 仍逐个按源码顺序处理实参并保留 NaN/Infinity 规则。
- `GetMember` 的右侧为 Number 且对象是普通数组时，直接查 `ArrayObject::elements`；只命中自有元素，缺失时回退原型链。静态 `.length` 走 `GetMemberConstKey` 和普通属性读取。`SetMember` 的动态 Number 索引只在普通数组已有可写数据元素时直接写 `ArrayObject::elements`；空洞、访问器、只读元素和越界写入仍走通用 `set_property`，以保留原型 setter、扩展性与错误语义。
- 通用属性读取每层复用已取出的堆对象，只对 TypedArray 分类数值索引。普通自有属性缺失才继续查原型；TypedArray 的规范数值索引未命中直接返回 `undefined`。自有属性描述符查询则把两种未命中都视为无描述符。普通对象的属性表在 8 个键以内用按 key 排序的密集向量，超过后退回 `BTreeMap`；`SetMember` 站点预热 32 次后缓存接收者、键、布局和槽，只用于普通对象的可写数据属性，值仍实时读取，布局变化或写失败回退通用 `Set`。读路径与恢复后的缓存都为空；缓存不进快照，也不改变快照 JSON 形状。

## 5. 关键文件

目录职责：

- `crates/vm/src` — 字节码契约、加载校验、解释器、堆与对象表、Promise/微任务、宿主协议、快照
- `crates/compiler/src` — Oxc 前端、降低、绑定逃逸、活性分析、槽分配
- `crates/host-dts/src` — 宿主 `.d.ts` 清单：每个字符串模块块下的直接值导出，以及带位置的拒绝
- `crates/binding-macros/src` — 类型化宏；`host!` 读取 `dts` 并在展开期校验声明名
- `crates/compiler/src/lower` — 唯一的函数体 walker：`stmt`、`expr`、`control`、`pattern`
- `crates/harness/src` — `TestHost`、指令用例运行器、演示二进制
- `crates/harness/tests/e2e.rs` 与 `crates/harness/tests/e2e/` — 手写的快照/恢复、加载器与 GC 测试
- `crates/harness/tests/cases` — 语义 `.js` 夹具；每个生成一个 `#[test]`

关键文件：
- `Cargo.toml` — workspace 成员与锁定依赖（`oxc = "=0.159.0"`，启用 `ast_visit`）
- `crates/compiler/src/lib.rs` — `Compiler` 与 `Project`；`transform.rs` 定义公开转换接口
- `crates/host-dts/src/lib.rs` — `parse(source) -> module/export name tables`，带位置的拒绝
- `crates/vm/src/lib.rs` — 再导出：`Build`、`Vm`、`Host`、`Snapshot`、`Value`、`VmError`
- `crates/vm/src/bytecode/instr.rs` — `instruction_table!` 是唯一的指令表，由同一份表生成 `Instr` 和穷尽的验证分派；变体顺序与字段顺序即产物编码
- `crates/vm/src/bytecode.rs` — `Instr`（再导出）、`ResumeEntry`、`GeneratorEntry`、`ExceptionHandler`、`Build::to_bytes` / `Build::from_bytes`
- `crates/vm/src/bytecode/validate.rs` — `Build::validate`，检查模块、函数、指令与续体表
- `crates/vm/src/vm.rs` — `get_export`、`start`、`run_until_idle`、`complete_operation`、`collect_garbage`
- `crates/vm/src/init.rs` — VM 构造与模块初始化：`Vm::new`、`VmOptions`、`prepare`、导入链接（`resolve_imports` / `install_imports` / `restore_imports`）与模块图实例化、求值
- `crates/vm/src/snapshot/` — `Vm::snapshot`、`Vm::restore`、恢复选项与快照编解码
- `crates/vm/src/gc.rs` — tracing GC 的根与可达性遍历；堆边统一由 `Heap::write_edges` 提供
- `crates/harness/src/case.rs` — `// @` 指令运行器
- `crates/harness/build.rs` — 生成 `OUT_DIR/cases.rs`
- `crates/harness/src/main.rs` — 唯一二进制；内嵌设计文档示例

## 6. 代码约定细节

- 新增 opcode：先在 `vm/src/bytecode/instr.rs` 的指令表末尾追加一项（变体、字段与顺序即产物编码）。`slots[...]` 按列出顺序检查槽位范围；`custom` 需要在 `Build::validate_function` 的 `validate_custom!` 中提供规则，按表中字段顺序接收绑定，缺规则时无法编译。再改 `compiler/src/instr.rs`（uses/defs/throws/rewrite）与 lowerer。新增控制流指令还需更新 `cfg.rs` 和 `cfg/layout.rs`；优化不遍历字节码 PC。
- 版本号：不兼容的字节码改动升 `BYTECODE_VERSION`（`crates/vm/src/bytecode.rs`）；快照记录形状改动升 `SNAPSHOT_FORMAT_VERSION`（`crates/vm/src/snapshot/record.rs`）；产物容器、magic 或 bincode 编码选项改动升 `BUILD_FORMAT_VERSION`（`crates/vm/src/bytecode.rs`）。三个契约各自独立，只有对应契约变化才升。`Build::finalize` 用 `compute_id` 设置 `build_id`；内容变化本身已使旧快照被拒绝，`build_id` 不是版本号的替代。`build_id` 与绑定指纹使用 xxh3-128 这一稳定的非加密摘要，只检测意外损坏和契约错配，不提供对抗篡改的安全性。构建摘要输入使用 bincode 固定整数、小端编码；编码选项与 crate 版本属于 `build_id` 契约，绑定指纹仍使用规范化文本。
- 错误类型：`CompileError` 是带位置的结构体（`message`、`line`、`column`），不是 `thiserror` 枚举。`VmError` 是 `thiserror`（`InvalidBuild`、`InvalidSnapshot`、`MissingImport` / `MissingOperation`、`Host`、`HostFault`、`UnknownOperation`、`InvalidState`、`Thrown`）。脚本抛出是 `Value`（`ExecOutcome::Thrown`），不是 `VmError`，直到它拒绝某个异步结果。宿主绑定函数返回 `JsResult`（`JsError::Throw` / `JsError::Fault`）；宿主调度失败用 `HostFault`。可选的 `Rc<dyn Diagnostics>` 接收未取消的全局异常与拒绝报告；模块初始化异常在 `Vm::new` 返回原 `VmError::Thrown` 前以 `Diagnostic::StartupException` 报告，不派发脚本事件。
- 有序映射一律 `BTreeMap` / `BTreeSet`（imports、globals、operations、natives）。身份或快照顺序相关时保持这一点。
- `const` 的可变性按绑定 ID 在编译期解析，包括经闭包。初始化写入（含循环声明）允许。对 `const` 赋值在写入处抛出可捕获的运行期 `TypeError`，位于此前求值副作用之后；逻辑赋值可以短路而不写入。这不会冻结绑定值。当前函数的静态 TDZ 状态在 `compiler/src/env.rs` 的 `pending_lexicals`，不在 emitter 中。闭包不继承该集合，因此跨闭包的提前读取得到 `undefined`。这不是完整 ECMAScript TDZ。`for` 的 `let` / `const` 头（C 风格、`for…in`、`for…of`、`for await…of`）每轮建立独立环境：`NewIterationEnv` 从当前环境复制仍逃逸的循环绑定到以保存的外层环境为父的新环境，`RestoreEnv` 在循环结束后恢复外层环境。未逃逸的循环头绑定由 `compiler/src/promote.rs` 提升为帧槽并从 `NewIterationEnv` 的绑定表移除；绑定表为空且循环体没有仍需独立单元格的被捕获声明时，整组环境指令被删除。循环体内的被捕获块级声明仍要求每轮新单元格，因此即使循环头绑定全部提升也要保留空绑定表的 `NewIterationEnv`。
- 名字解析（`compiler/src/names.rs`）：本地绑定、静态导入（源码与宿主）、合格的 preload 全局，最后是内建及三个全局事件处理器属性。裸 preload 名在 preload 子图之外经 `globalThis` 读取只读 getter；处理器名经固定 `globalThis` 读写动态属性，裸调用不绑定接收者。
- 宿主模块使用排序的 `Compiler.imports: BTreeMap<String, BTreeSet<String>>` 清单。`Compiler::from_dts` 从宿主 `.d.ts` 读出它；`host! { dts: "path", modules: […], services: {…} }`（相对路径按宏调用所在源文件解析，无法确定调用文件时回退到 `CARGO_MANIFEST_DIR`）在宏展开时重新解析同一文件，把每个声明的 `module:export` 与同一模块类型的 `HostModule::NAME` 与 `ModuleExports::EXPORT_NAMES` 比对。声明了但 `#[module_exports]` 未实现的名字在编译期报 `missing host export: module:export`；多出的实现允许存在，且不进入命名空间导出。Harness 声明是 `crates/harness/test.d.ts` 与 `crates/harness/demo.d.ts`；e2e 夹具在 `crates/harness/tests/dts/`，按排序后的模块组合命名。
- `TestHost` 是测试夹具，`host.host()` 为新建或恢复构造新的 `Host`，显式复用测试服务句柄。导出名单以 `crates/harness/test.d.ts` 为准；`seed()` 返回 `10`；`wait(label)` 启动 `Wait` operation（显示标签 `wait:{label}`，恢复策略 `Resume`）；`bind(label)` 分配带 payload `label` 与 `label()` 方法的 `Node` 实例；`expect(actual)` 分配带槽 `actual` 的 `Matcher` 实例；`inspect` / `setLabel` 返回 `undefined`。用 `host.labels().set_reject_restore_operation(true)` 注入恢复后的 operation 启动失败；通过编辑 record 破坏快照 payload。
