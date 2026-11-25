# Fetch：完整脚本语义与宿主传输契约

> 本文是[总体设计](../design.md)的专题文档。WinterTC 的 API 范围、全局暴露、相对时钟与可选宿主能力见 [wintertc.md](wintertc.md)。

目标是实现 [WHATWG Fetch Standard](https://fetch.spec.whatwg.org/) 中适用于该运行时的 Fetch API 和获取算法，不是只提供 buffered HTTP 便捷函数。按以下边界分工；同一适配器可包装平台 HTTP 客户端，不需要实现第二套 JS Fetch。

## VM：Web IDL、对象与获取算法

- 实现 `Headers`、`Request`、`Response` 及 Body mixin 的完整公开契约：参数转换与验证、header list 的字节和重复项语义、guard 和不可变 header、`getSetCookie()`、请求默认值和方法规则、`Request` 的信号关联、`Response.error/redirect/json`、类型和 URL 元数据。VM 管理 body 的 null／字节流区分、locked／disturbed／`bodyUsed`、clone/tee、`arrayBuffer`、`blob`、`bytes`、`formData`、`json`、`text`，并实现规范的 MIME／编码、multipart 和 `BodyInit` 提取（包括流式上传与 `duplex`）。`Blob`、`File`、`FormData` 和脚本合成的 Request/Response 不依赖网络。
- VM 驱动 Fetch 算法：解析有效 URL，处理 `about:blank`、`data:` 及规范可解析的 `blob:` 本地资源，缺少资源或不支持的 scheme 走网络错误（不把 `file:` 映射到宿主文件系统）；逐跳处理 `redirect`、URL 列表、重定向上限、方法／请求体变换、不可重放上传和跨 origin header 清理。按规范处理 `mode`、CORS（含预检）、`credentials`、HTTP cache mode、referrer/referrerPolicy、`integrity`、`keepalive` 限制，以及适用的 HTTP 子算法；产生 basic／CORS／opaque／opaqueredirect 过滤结果，不将内部响应泄漏给脚本。HTTP 4xx／5xx 是正常响应；网络、策略与完整性失败按 Fetch 算法映射为网络错误，不伪造 HTTP 状态。
- Fetch 环境由宿主在创建 VM 时显式配置，包含可选基准 URL（即 [WinterTC URL](wintertc.md) 的 baseURL，`URL` 构造器与 Fetch 共用同一值）、origin、referrer 和网络权限；影响恢复语义的环境参数随 VM 保存并在恢复时核对，不从启动机器猜测。无基准 URL 时不接受相对字符串 URL；未配置 origin 则使用不透明 origin，按相应规范处理同源判断、CORS 和 `Origin` 序列化。按条件使用默认 User-Agent。无 `Window`／导航、Service Worker 或嵌入文档：不模拟这类环境的拦截、文档策略或页面卸载；记录浏览器上下文不适用的路径，不静默忽略本运行时适用的安全检查。`keepalive` 不承诺跨 VM 销毁或快照恢复继续传输。

## Host：单跳 HTTP、背压与环境服务

Rust 嵌入方通过 `host!` 将 `Arc<dyn FetchService>` 注册到交由 VM 接管的宿主。平台工作在 VM 外执行，事件由嵌入方回到 VM 线程调用 `vm.deliver_fetch_event` 投递，不再另传宿主，也不在平台回调中同步执行脚本。

- 最小 HTTP 适配器接收 `start(token, HopRequest) → registration`，其中含绝对 URL、方法、保序的 header 字节对、无体／已知长度／未知长度上传描述及本跳的凭据约束；登记提供 `send_upload(bytes)`、`finish_upload()`、`grant_download(bytes_limit)`、`cancel()`。名称仅示意接口方向，不锁定 ABI。单跳指 HTTP 交换，不指整次 Fetch；重定向、预检与后续请求由 VM 另发，平台不得自动重定向、应用浏览器响应过滤、合并 `Set-Cookie` 或擅自重试已可能发送的请求。适配器负责 DNS、连接、代理、TLS、HTTP framing 等平台工作；单跳传输输出去 framing、未自动解压的字节，由 VM 按适用规则解码，避免重复解压或丢失 header 信息。
- 投递类型限定为上传额度、最终 `ResponseHead { status, status_text, headers }`、响应字节、EOF 或类型化失败；需要观测的中间响应保留为单独事件。上传按宿主额度从原 VM 流读取并送入适配器，下载按 VM 授予的字节额度投递，不能以无界缓冲替代流。额度和字节所有权、单跳事件顺序、唯一终结及取消明确写入协议；最终 headers 可先于上传 EOF，最终响应体按规范在可暴露时交付，不要求提前读完。`AbortSignal` 或 body 取消由 VM 按 Fetch/Streams 算法转成取消，事务终结后忽略迟到结果。普通网络失败在响应交付前拒绝原 Promise，交付后只使相应未结束的体流出错；中止遵循规范的 reason 与阶段，不以宿主错误字符串替代脚本原因。
- 每个实际出站的 URL（含重定向、预检和连接重试）都受宿主授权及连接层策略约束；不能把 CORS 当作宿主网络权限。平台客户端应禁止隐式共享 cookie、认证和缓存。可按需注入隔离的 cookie／HTTP 认证／缓存服务，并遵循 VM 为每一跳传入的 origin、分区及 credential/cache mode；无可选存储时是空凭据库、缓存 miss 或按模式返回网络错误，不降级 `only-if-cached` 为联网。宿主可配置请求体、响应体和并发资源限制，超限走可诊断的错误，不能截断成成功。平台服务是外部环境，不是保存 VM Fetch 进度的第二份存档。

## Fetch 快照与验收

在一致切点保存原 VM Promise、Fetch 状态、已接入 VM 的响应和流关系、排队的交付及取消状态；不保存 socket、TLS 会话、平台请求 ID、宿主缓冲或原生回调。捕获快照不取消源 VM 的登记；源 VM 停止时由其宿主释放登记，恢复实例不依赖源 VM。已完整进入 VM 的响应在恢复后继续由 VM 消费；涉及活动外部传输的恢复基线是**中断，不重发**：响应交付前以 `AbortError` 拒绝原 Promise，交付后不改变已兑现结果，使仍依赖传输的流按中断出错，并清理上传源和取消监听。此终结只在恢复发布后按任务顺序发生；若切点有已归入 VM 的终结事件，先按记录顺序消费，不重复结算。旧代际的迟到结果无效；不能取得一致切点则拒绝捕获。请求方法、已发送字节或 host operation ID 均不作为重放安全证明。

先以可控传输验收一次单跳接入，再运行适用的 WPT Fetch 测试及本地 HTTP 测试：覆盖 header guard 与重复值、body 消费／clone、慢速双向流与背压、预检和各重定向模式、凭据和响应过滤、缓存／完整性错误、缺能力及中止竞态。分别在上传中、等待响应头、响应头已交付、读体中和 EOF 后做快照往返；断言原 Promise 或流按阶段终结、已完成的体可读、宿主没有重复请求。文档与测试要区分脚本侧规范语义、宿主未提供能力以及有意的运行时偏差。
