# Web API 互操作性设计：WinterTC

> 本文是[总体设计](../design.md)的专题文档：WinterTC 目标范围、VM 相对时钟、可选宿主能力、随机 API 与定时器。Fetch 的完整契约见 [fetch.md](fetch.md)。

以 WinterTC 的 [ECMA-429 第一版（2025 年 12 月）](https://ecma-international.org/publications-and-standards/standards/ecma-429/) §§5–7 为固定基线：**除 WebAssembly 外，列出的所有接口、全局方法和属性均为实现目标**。目标包括上游规范定义的成员及可观察行为，不只是同名全局；解码标签范围按下表有意收窄。纯 VM 功能不依赖宿主接入；有外部依赖的功能仍提供 JS 接口，由可选宿主能力承接外部工作。此目标不改变首版框架验收清单，也不声明 ECMA-429 合规（该标准还要求完整的 ECMA-262 合规）。

| API 组 | 实现目标与归属 |
| --- | --- |
| 事件与取消 | `EventTarget`、`Event`、`CustomEvent`、`DOMException`、`AbortController`、`AbortSignal`。监听器、取消原因与信号关系由 VM 持有；`AbortSignal.timeout()` 复用宿主定时器能力。本 API 组不引入 DOM 节点或事件树；Web 宿主子集的 DOM 由独立宿主绑定提供。 |
| 全局错误与拒绝 | `reportError`、`ErrorEvent`、`PromiseRejectionEvent`、`onerror`、`onunhandledrejection`、`onrejectionhandled`。目标全局对象由 VM 实现 `EventTarget`，支持全局监听及上述处理器属性；遵循异常报告和 Promise 拒绝跟踪的通知时机及取消规则。宿主日志是报告出口，不替代脚本通知。 |
| 消息与克隆 | `structuredClone`。补齐规范所需的克隆与转移类型（如 `Blob`、`File`），不要求 Worker。`MessageChannel`、`MessagePort`、`MessageEvent` 暂不作为开发目标：端口配对与任务投递在可恢复 VM 的语义下不易实现（挂起任务与快照恢复边界难以定义），留待后续重新评估。 |
| 定时器与时钟 | `setTimeout`、`clearTimeout`、`setInterval`、`clearInterval` 由 VM 实现脚本语义，由可选宿主能力唤醒。`Performance`、`performance.now()`、`performance.timeOrigin` 完全由 VM 提供。禁止运行时编译，故字符串 timer handler 不支持。 |
| 全局别名与标识 | `globalThis`、`self`、`navigator.userAgent`、`navigator.platform`。`self` 初始值与 `globalThis` 相同；`userAgent` 由宿主提供，默认按 §7 使用符合 HTTP 语法的运行时标识，不假冒浏览器，也不决定 Fetch 出站的 `User-Agent` 头；`platform` 是二进制编译的目标平台 `<target_os>-<target_arch>`。 |
| 编码与 Base64 | `atob`／`btoa` 按 HTML 字符串 API 实现。`TextEncoder` 和 `TextEncoderStream` 仅输出 UTF-8；`TextDecoder` 和 `TextDecoderStream` 仅支持 UTF-8、UTF-16LE、UTF-16BE 及其在 Encoding Standard 中定义的标签，其他标签抛 `RangeError`，不纳入实现目标。支持范围内遵循 Encoding Standard 的增量处理、BOM 和错误语义；两个流接口提供 `readable`／`writable`，增量编解码状态随 VM 快照恢复。 |
| URL | `URL`、`URLSearchParams`、`URLPattern`。覆盖 URL 解析与序列化、`URL.searchParams` 的双向联动及模式匹配。基准 URL 不来自浏览器文档：创建 VM 时由宿主显式提供一个 baseURL（可省略），它是 `new URL(相对URL)` 省略 `base` 实参时的默认基准，也用于 Fetch 相对 URL 解析；省略 baseURL 时无默认基准，相对 URL 按规范抛出 `TypeError`。 |
| 微任务、流与压缩 | `queueMicrotask`；`ReadableStream`、`ReadableStreamDefaultReader`、`ReadableStreamDefaultController`、`ReadableStreamBYOBReader`、`ReadableStreamBYOBRequest`、`ReadableByteStreamController`；`WritableStream`、`WritableStreamDefaultWriter`、`WritableStreamDefaultController`；`TransformStream`、`TransformStreamDefaultController`；`CountQueuingStrategy`、`ByteLengthQueuingStrategy`；`CompressionStream`、`DecompressionStream`。流的队列、锁、背压、待结算 Promise 和增量编解码进度进入 VM 快照。 |
| Fetch 与二进制对象 | `Headers`、`Request`、`Response`、`FormData`、`Blob`、`File`、`fetch`。`FormData` 不引入 HTML 表单元素；VM 实现 Fetch 算法和 JS 对象，外部网络及环境服务通过可选宿主能力接入，见 [Fetch 设计](fetch.md)。 |
| Console | 全局 `console` 的规范方法、参数处理、分组与计时状态由 VM 管理；输出交给可选宿主 sink，恢复不重放日志。 |
| Web Crypto | 目标接口为 `crypto`、`Crypto`、`CryptoKey`、`SubtleCrypto`。VM 实现参数、类型、用法与可导出性约束，熵源及密码操作由独立的可选宿主能力提供。可导出性与快照可恢复性独立；不可导出密钥不以明文或进程句柄落盘，无法安全恢复的可达密钥应使快照失败。 |
| WebAssembly（范围外） | 缺少整个命名空间，包括 `Global`、`Instance`、`Memory`、`Module`、`Table`、`Tag`、`Exception`、`CompileError`、`LinkError`、`RuntimeError`，以及 `compile`、`compileStreaming`、`instantiate`、`instantiateStreaming`、`JSTag`、`validate`。仅记为标准差异，不纳入当前引擎设计范围。 |

- **全局暴露。** 不引入 `Window`、`WorkerGlobalScope` 或 Worker。非 `[Exposed=*]` 接口在 VM 自有作用域暴露是相对 §6 的设计偏差。新增全局同步纳入 `globalThis` 和静态裸名解析，保留局部绑定遮蔽规则，不把宿主属性动态注入编译器名字表。
- **快照边界。** JS 身份、监听器、流状态和已接收但未运行的普通任务数据由 VM 持有，快照不保存 OS 句柄、原生调用栈或待执行的 VM 微任务。外部操作必须定义中断、交付与恢复；无法捕获一致状态时拒绝快照，不得静默丢弃或重放外部副作用。
- **验收。** 各 API 对照上游规范及适用的 [Web Platform Tests](https://github.com/web-platform-tests/wpt)；另覆盖快照前后身份、任务／微任务顺序、取消、缺失能力和恢复不重复交付。逐项记录实现覆盖与可观察偏差。

## VM 相对时钟

`performance.now()` 返回 VM 内部单调计时器的毫秒数，支持小数；不查询宿主能力，也不依赖系统墙钟。新 VM 从零开始。运行期间（含 VM 等待外部事件）累计本地单调时间；快照只保存在一致切点采样的累计时间，不保存进程单调时钟锚点。恢复以该累计值为基数重新锚定，存档至恢复之间的时间不计入。`performance.timeOrigin` 始终为 `0`；它不是 Unix epoch 或任何绝对坐标，`timeOrigin + now()` 仍只是 VM 内的相对时间，不能用于跨 VM 或跨进程对时。

同一个 VM 计时域也用于 timer 截止时间和 Console 计时。快照恢复不得倒退或重置时钟；宿主唤醒只使用相对延时，不是 JS 时钟的权威。验收使用可控的内部测试时钟，覆盖分数精度、单调性、恢复时间连续性及存档间隔不计入。

## 可选宿主能力

启动 VM 时以独立的类型化服务注册定时器、Fetch、Console 输出和 Crypto 等能力，复用现有 service / operation 生命周期，不要求脚本导入宿主模块、不要求宿主实现全局对象或字符串分派。语言无关的边界传递有类型的记录、字节和代际令牌；宿主不持有脚本函数、`JsValue`、Promise 或流对象，也不从平台回调同步重入 VM。宿主可以在后台线程执行工作，仅在 VM 线程按任务投递；每个脚本任务后进行微任务检查。服务声明可版本化，活动快照记录声明所需的兼容版本；不保存服务实例。恢复时需重新登记的 operation 缺少匹配能力则在发布前报错；有纯 VM 终结策略的记录不必重连服务。

能力默认关闭，不回退到进程的全局网络客户端、凭据库、文件系统或熵源。全局名字仍存在；需调用外部服务的同步 API 缺能力时抛出可捕获的异常（定时器、Crypto 或 Console 输出默认 `NotSupportedError`），Promise API 按正常时序拒绝（网络 Fetch 为网络错误 `TypeError`，异步 Crypto 为 `NotSupportedError`）。无外部工作时按原 API 行为，例如清除不存在的 timer 是空操作，`new Response()` 和本地 `data:` Fetch 不依赖网络能力。宿主拒绝权限或操作失败由 VM 映射到对应 API 错误；过期令牌、越权投递及协议违约是宿主集成错误，不当作正常结果。

### Web Crypto 随机 API

`crypto` 全局始终存在。宿主可通过 `Bindings` 在稳定 ID `wintertc.crypto` 注册 `Arc<dyn CryptoService>`；同步 `fill_random(&mut [u8]) -> Result<(), HostFault>` 成功时须填满切片，既不接收脚本值，也不重入 VM。两种随机方法仅在调用时查找服务，不回退到 `Math.random` 或进程全局熵源。无匹配服务（包括类型不符）时抛出 `NotSupportedError`；宿主失败时抛出 `OperationError`，不提交部分写入。快照不保存服务实例、熵源状态或内部临时字节；脚本保留的返回值和缓冲区仍按普通值保存。

### 定时器

- 宿主只需实现一次性 `arm(token, relative_delay) → registration` 与可取消登记；到期投递 token，不分配 JS 定时器 ID、不执行回调，也不负责周期运算。VM 实现延时转换、HTML 的嵌套最小时延、`setTimeout` / `setInterval` 共用 ID 表及 `clear*` 交叉取消；`AbortSignal.timeout()` 也通过同一唤醒能力驱动。提前唤醒按 VM 时钟重新登记，已取消或重复唤醒忽略。
- VM 保存逻辑 ID、回调和参数引用、种类、周期以及同一相对时钟域的下次截止时间和任务顺序；interval 每次执行后仅在未取消时登记下一次，并按本轮调度，不补跑存档期间的 tick。恢复按保存的截止时间及恢复时钟重新计算相对延时，已到期则入普通任务队列。旧平台登记的迟到投递不会触发新代际回调；切点只接受已纳入 VM 状态的任务或仍可由逻辑记录重建的登记。

## Fetch

Fetch 的 Web IDL 对象、获取算法、单跳宿主传输、背压、环境服务、快照与验收契约单独成文：[fetch.md](fetch.md)。
