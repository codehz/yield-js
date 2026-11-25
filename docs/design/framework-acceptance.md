# Web 宿主与框架验收

> 本文是[总体设计](../design.md)的专题文档：用锁定版本的真实框架产物验收 Web 子集宿主能力、运行行为与快照恢复。构建输入见 [build.md](build.md)，语言子集见 [language.md](language.md)。

- 宿主提供 Web 客户端子集的 DOM 风格树：创建节点/文本、设置属性/文本、插入/移动/删除、遍历父子兄弟、分发输入事件；CSS 等样式资源由宿主消费。节点以绑定实例表示：节点间和事件回调的引用走 VM 托管槽，重建平台节点所需的非 JS 数据随实例写入快照。当前用自建内存 renderer 实现等价的 mutation 操作，作为过渡验收宿主；最终目标由 Blitz 一类 Web 渲染引擎承接。不提供 Node 或 React Native 全局环境，也不要求完整浏览器外壳（导航、历史、Service Worker、扩展、浏览器 UI）；完整 HTML 解析与 CSS 布局属于宿主，不属于 VM。
- React 的生产 `scheduler` 选用 [`setTimeout` 回退分支](https://unpkg.com/scheduler@0.27.0/cjs/scheduler.production.js)。框架验收宿主须接入可取消的 timer 唤醒；`performance.now()` 由 VM 提供，恢复延续同一相对时钟。定时器回调由 VM 在独立任务轮次执行并排空微任务，回调抛错走全局错误报告；取消或重复投递不执行回调。树事件也在新任务轮次投递，不从原生调用栈同步重入 VM。此验收要求不等于所有宿主都必须提供定时器。
- **运行验收**：用冻结版本的真实生产包和实际 JSX 构建产物。Web 目标使用 `react@19.2.0`、`react-dom@19.2.0` 的 `react-dom/client` 和 `scheduler@0.27.0` 生产入口；React DOM 自带 reconciler，不要求应用直接依赖 `react-reconciler` 或自定义 `HostConfig`。React 验证函数组件、`useState`、effect 及清理、带 key 的子节点重排、Promise 驱动的 Suspense 恢复和卸载。与参考 JS 引擎以 ESM 模式执行同一产物、遵循同一 Web 宿主适配器契约时的节点树和事件日志对照。自建 renderer 阶段可先用自定义 `HostConfig` 验证 VM 与快照，但不得把它作为最终兼容接口；最终验收必须使用按 Web 宿主声明构建的产物，在 Web 宿主（预期 Blitz）上完成，过渡 renderer 的产物不作为最终验收对象。
- **恢复验收**：分别在已挂载、等待可恢复 operation 的边界保存快照。用新的宿主从快照重建节点后投递事件或结算原 operation。断言脚本可见的节点和闭包身份延续、挂载副作用不重跑、后续更新和清理正常；React 另验证挂起的 scheduler timer 恢复后只触发一次、已取消 timer 不执行。不能用重新挂载代替恢复。Web 宿主必须按快照中的绑定 payload 重建节点，不依赖旧 DOM 或旧渲染器句柄。
