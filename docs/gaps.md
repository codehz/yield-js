# 实现缺口

> 对照 [`design/language.md`](design/language.md) 的首版语言语义与 [`design/build.md`](design/build.md) 的构建输入，由当前 `crates/compiler` + `crates/vm` 推导。已实现范围见 [`implemented.md`](implemented.md)。
>
> 语法子集拒绝报告带行列的 `unsupported syntax: …`，与无效 JavaScript 的语法诊断不同。宿主或 VM 故障不是脚本异常缺口。

## 合法输入的编译期拒绝

以下是合法 JavaScript，在此子集中于编译期拒绝，不是解析失败。

- 正则字面量由 Oxc 解析后经 VM 匹配器校验；匹配器不支持的模式报告 `unsupported syntax: …`，无效模式或标志也可能先被 Oxc 拒绝。支持的标志与语法见下文 `RegExp`。
- 同一函数内对尚未初始化的词法绑定的部分访问（如 `x; let x;`）在编译期拒绝，而不是运行时抛出可捕获的 `ReferenceError`。完整 TDZ 见[首版范围之外](#首版范围之外)。
- 模块初始化帧是同步的：顶层支持 `using`，拒绝 `await` / `await using`；嵌套块不能绕过。
- 默认文件系统解析只支持 ESM。显式 Web 生产解析只支持静态 CommonJS 子集：不支持动态 `require`、`require` 逃逸、`require` ESM，或无法静态确定的导出形状。两种模式都不支持 JSON 模块、Node 内建、`module` 回退、`exports` 通配符或数组、跨安装边界的符号链接。
- Node 扩展名加载器只接收 UTF-8 文本，不直接加载二进制。内嵌 preload 不是虚拟文件系统，不向普通导入注册别名。
- JSX 只在 `.jsx` 模块中启用；不支持 JSX 命名空间或 spread 子节点。
- 自定义 AST 转换锁定当前 Oxc 版本，没有跨版本兼容层。图转换引入的源码依赖必须已在图中；需要发现新模块的转换应在源码阶段运行。

## 内建与宿主 API 限制

当前限制，不是首版承诺或完整 ECMAScript 清单。已提供的全局与方法见 [`implemented.md`](implemented.md)。

| 内建 | 限制 |
| --- | --- |
| 异步迭代 | `for await…of` 的同步回退由编译器展开，不分配可反射的 `AsyncFromSyncIterator`；没有其它内建异步迭代器包装。每轮只对 `value` 做隐式 `await`。 |
| `Array` | 无 `Array.prototype.toLocaleString`。`flat` 遇到无法减少深度的循环数组抛 `TypeError`，不会无限展开。 |
| 错误构造器 | `stack` 只含创建时仍在执行的同步脚本帧：不含 Rust 宿主帧，也不回溯 `await` / `yield` / 微任务之前的调用。 |
| `DOMException` | 只提供 Web IDL 异常对象和历史常量，不附带浏览器 DOM API。 |
| 事件与取消 | 无 `Node`、`ShadowRoot`、activation 或 retargeting。全局错误位置只取报告时可见的同步脚本帧。 |
| 定时器与时钟 | 不支持字符串 timer handler、Performance Timeline 或 User Timing。`performance.timeOrigin` 是 VM 相对时间域的零点，不是 epoch。全局 timer 和 `AbortSignal.timeout` 需要宿主唤醒服务。 |
| `navigator` | 只有 `userAgent` 与 `platform`。无 `Navigator` 原型、语言、硬件能力、在线状态等 HTML 成员。`platform` 是编译目标 `<target_os>-<target_arch>`，不是浏览器平台字符串。 |
| Web Crypto | 子集：随机方法，以及 `crypto.subtle` 的 SHA-256、raw 密钥导入/导出、PBKDF2-SHA-256 派生 AES-GCM、AES-GCM 加解密、HMAC-SHA-256 签名/校验。无全局 `Crypto` 接口对象；无不可导出密钥（PBKDF2 只接受本实现的可导出 raw 密码）；无其它格式、算法或用途，也无 `generateKey` / `deriveBits` / `wrapKey` / `unwrapKey`。原语完成句柄不唤醒 VM；有待交付请求时宿主需再调 `run_until_idle`。 |
| `Number` | 非十进制 `toString` 只转换整数部分。`toLocaleString` 等同默认十进制 `toString`，无区域设置。 |
| `BigInt` | `toLocaleString` 等同十进制 `toString`，无区域设置。 |
| `Date` | 无 Intl；`toLocale*` 复用本地日期或时间格式。`parse` 只接受标准 Date Time String Format 以及自身 `toString` / `toUTCString` 的输出。`GMT±HHMM` 不含偏移秒数，解析这类历史本地字符串时用当前宿主时区补回秒数。未注册 `time`（`TimeService`）时使用固定 epoch 0 与 `Etc/UTC`。 |
| `Math` | 浮点函数依赖 Rust `f64`。`random` 是固定种子的非加密 PRNG。 |
| `RegExp` | 语法由锁定版本的 `regress` 决定；标志 `d` / `g` / `i` / `m` / `s` / `u` / `v` / `y`（`u` 与 `v` 互斥）。无 Annex B 的 `compile` 和旧式静态匹配属性；不保证覆盖全部 Annex B 语法。 |
| `Map` / `Set` | 查找、删除与集合运算按活槽线性扫描；删除保留空槽。 |
| `Proxy` | 私元素与宿主隐藏槽不走 trap。快照保存目标与 handler，不经 trap 查询。已撤销代理的内部操作抛 `TypeError`。 |
| `ArrayBuffer` / `DataView` / TypedArray | `toLocaleString` 复用 `Number` / `BigInt` 的同名方法，无 Intl 文本。无 `SharedArrayBuffer`、`Atomics`、分离后重新附加。 |
| `TextEncoder` / `TextDecoder` | 解码只支持 UTF-8、UTF-16LE、UTF-16BE 及其标准标签；其它标签（如 `gbk`）抛 `RangeError`。对应 Stream 变体使用同一规则。 |
| `URL` / `URLSearchParams` / `URLPattern` | 未跑完整上游 WPT。对象 URL 表只在本 VM 内可见。 |
| Fetch 与二进制对象 | `FormData` 不读取 HTML 表单。无 TLS、代理、cookie jar、HTTP 缓存或预检结果缓存（每次跨源非简单请求重新预检）。示例适配器只允许显式配置的 `127.0.0.1` 明文 HTTP/1.1；HTTPS 需其它适配器。`keepalive` 不跨 VM 销毁或快照恢复继续传输；恢复时在途传输按设计中断且不重发；`only-if-cached` 因无缓存总是 miss。 |
| `structuredClone` | 可转移 `ArrayBuffer` 与三类 Stream（流只在同一 VM 内转移）。不克隆函数、代理、模块命名空间、非错误宿主实例等不支持的种类。宿主错误克隆为普通 `Error`。不复制 VM 执行环境。 |

未提供的全局：`SharedArrayBuffer`、`Atomics`、`Intl`。`Temporal` 与 `Iterator.zip` / `Iterator.zipKeyed` 不属于 ES2026。

### yield-blitz

- 接入原生主键点击、复选框激活和单行文本编辑。没有完整浏览器 DOM、`MouseEvent` 坐标与按键字段、`InputEvent` 专有字段、键盘或组合事件的 JS 派发、完整表单控件绑定、样式计算 API 或热更新。
- DOM payload 保存当前绑定所需的 Document、Element、Text、Comment 树，以及文本框当前值和复选框当前值 / dirty 状态。不保存布局、渲染器、焦点、选区、未提交的输入法文字或未投递的原生输入；不支持模板内容子树。无窗口测试不等于 Vello 目视验收。
- 默认不自动跑 VM GC。嵌入方应在排空微任务后调用 `BlitzSession::collect_garbage`，并传入仍持有的 VM 值作为根。不跨检查点持有 VM 值的调用方可像窗口演示一样启用空闲回收；Blitz 不启用活动栈 GC。快照不会回收当前会话内存。
- 未注册专用原型的 HTML 标签使用 `HTMLElement`；不区分未知标签与标准通用 HTML 元素，也不覆盖废弃标签的专有原型。专用原型不代表元素特有行为已实现（媒体播放、表格操作、表单提交等）。
- 演示入口只编译 `src/blitz.jsx` 及其依赖。CSS Modules 不支持跨文件 `composes` 或命名导出；两类样式表都不在构建期解析 CSS 内的 `@import` / `url()`，也不支持动态样式模块。`compile_module` 是自包含源码入口，不访问文件系统。不验收 `src/index.tsx` 或 TypeScript 类型剥离。

## 结构性限制

1. **宿主绑定未决项。** 尚未实现 `JsMap` 槽、C ABI、TypeScript 定义生成、payload 流式/资源上限和跨线程交付。非空模块需显式选择初始化及 codec；普通 Rust 字段可派生 `Encode` / `Decode`。宿主异常可继承 `yield_vm::intrinsics` 提供的异常类，不接受任意内建构造器。已支持范围见 [`implemented.md`](implemented.md)。
   类、operation 与必需服务类型必须显式列出，不从导出签名或函数体推断；清单覆盖继承、间接调用及恢复路径。自定义服务的必需依赖在编译期检查；可选服务不列入依赖，由调用方处理缺失。
   运行时不能增删或替换服务，也不能声明可选依赖。`host!` 构造时确定布局；未注册任一声明依赖的宿主在链接时被拒绝。
   绑定指纹不检查保留 `.length` 的参数解码变化；恢复兼容不意味着宿主调用行为不变。
2. **宿主续体不能等待任意 Rust Future。** `loop` / `while` / `while let` 的循环体可挂起；`while` / `while let` 的条件和 `for` 循环不能挂起。Guest 调用与 GC 同步恢复；只有正在等待外部 operation 的宿主任务可写入快照。Rust 栈、函数指针和 `ModuleCx` 不序列化；模块续体只在分派时重获本模块的状态能力。
   宿主 rest 参数 `&[JsValue]` 只借用当前同步阶段的实参，不能跨挂起保存；跨阶段使用须先提取所需的 `JsValue`。
   `CallCx::call_sync` 只在活动绑定中调用脚本，不提供调度器重入、同步等待 Promise，或事件父级回调中的 guest 调用。
3. **GC 不会自动触发。** 嵌入方可在调用之间排空微任务后调用 `vm.collect_garbage(roots)`，或用 `CallCx::request_gc` 向脚本暴露同步收集。只有后者使用可选的 `gc_roots`（`GcRoots`）访问宿主持有的本 VM 值。没有默认的脚本 `gc` 全局函数。
4. **瞬时槽复用不是精确 GC。** 分配器使用覆盖所有活性位置与写入点的连续区间，不利用区间内部空洞。活动帧的 GC 仍扫描所有非空槽，未被覆盖的死引用可能继续保留对象。
5. **静态属性键不缓存读取。** 专用读取指令只跳过字符串键的槽读写和转换；`SetMember` 对普通对象的自有可写数据属性有写侧单态缓存。读取、计算键、复合赋值和 `super` 仍走通用属性路径。
6. **CFG 常量传播不折叠通用 JS 运算。** 只传播已有字面量和槽复制，并判断已知字面量的真假值。无跨函数常量传播或运行时类型专用化。
7. **快照不是不可信导入格式。** `Vm::restore` 默认 `SnapshotValidation::Trusted`，不重查受信镜像的堆图；应用负责来源、完整性和读入大小。`RestoreOptions::validation` 可设为 `SnapshotValidation::Full` 以诊断镜像不变量，但不提供认证、隔离或资源预算。

## 首版范围之外

这些不是相对首版设计的缺口。

- 完整 ECMAScript TDZ
- 运行时加载或编译代码。动态 `import()` 只接受字符串字面量，按依赖图提前求值，不保留惰性加载或原生动态导入时序。内建 `eval` 无论输入为何均抛 `EvalError`；调用或构造内建 `Function` 均抛 `TypeError`。
- 导入/导出属性在编译期拒绝。`import.meta` 只折叠宿主在打包期注入的完整静态路径常量，不提供对象、`resolve` 或运行时 URL 推导。跨模块读取有运行时 TDZ 检查，不改变普通函数的 TDZ 规则。
- `debugger`
- 共享内存、`Atomics`
- 旧式 `Object.prototype.__proto__` 访问器；保留 `Object` / `Reflect` 的显式原型操作
- 快照迁移：恢复要求 `SNAPSHOT_FORMAT_VERSION`、`BYTECODE_VERSION`、`build_id` 和宿主绑定指纹均匹配；二进制镜像还要求当前 `SNAPSHOT_BINARY_VERSION`。不做容器迁移。
- 产物迁移：`Build::from_bytes` 只接受当前 `BUILD_FORMAT_VERSION` 与当前 bincode 编码，不读取版本 1 的无资源容器或旧 JSON 产物。VM 不解析资源 URI 或内容，不加载或分派资源。

## 无效 JavaScript

以下写法违反 JavaScript 或严格模式规则。即使诊断由 lowering 报出，也不表示缺少对应特性。

- `for…in` / `for…of` 循环头的变量声明只能有一个声明项（其中可以解构多个绑定），且在严格 ESM 中不能带初始化器。
- 数组、对象解构赋值模式只能配 `=`；复合及逻辑赋值要求简单赋值目标，自增减也要求有效的赋值目标。
- 模块顶层及仅嵌套在其中的箭头函数不能使用 `new.target`。普通函数有自己的 `new.target`，嵌套箭头函数可捕获它。
- 同一个 `switch` 中重复 `default`，以及默认导出以外的匿名函数声明，都是语法错误。
- 普通函数、异步函数和模块体中不能使用 `yield` / `yield*`；它们只在 `function*` / `async function*` 中合法。
- 严格模式下不能使用 `with`（不影响 `Symbol.unscopables` 和数组名单的反射），也不能删除裸标识符（包括加括号的标识符）。
