# 构建输入与兼容基线

> 本文是[总体设计](../design.md)的专题文档：锁定验收目标、构建输入、扩展管线与资源处理。语言语义见 [language.md](language.md)，框架验收见 [framework-acceptance.md](framework-acceptance.md)。

## 兼容目标与构建输入

| 验收目标 | 锁定的运行时入口 | 不包含 |
| --- | --- | --- |
| Solid 非 Web | [`solid-js@1.9.9`](https://unpkg.com/solid-js@1.9.9/package.json) 的生产响应式核心、`solid-js/universal` 的 `createRenderer` 和 `solid-js/store` 的 `createStore`。构建显式选用 `dist/solid.js` 和 `store/dist/store.js`，不能因 `node`/`worker` 条件误选 SSR 入口。 | `solid-js/web`、SSR、浏览器 hydration；不启用依赖浏览器 `MessageChannel` 的可选 `enableScheduling()` 路径。 |
| React 非 Web | `react@19.2.0`、`react/jsx-runtime`、[`react-reconciler@0.33.0`](https://unpkg.com/react-reconciler@0.33.0/package.json) 和 `scheduler@0.27.0` 的生产包；使用自定义 mutation-mode `HostConfig`，按该版本实现调度与事件优先级接口。 | React DOM、React Native/Fabric 平台层、SSR/RSC、hydration 和开发工具。 |

应用入口可直接使用 JS/TS/JSX/TSX，并从源码按模块解析依赖。扩展编译器管理可达模块图；核心解析与链接层保留 ESM 语义，不要求先将依赖打成单文件。可组合的扩展管线负责模块解析/加载、语法转换和资源处理；核心提供默认 ESM 路径，扩展可分别替换或组合各阶段。项目提供可选的 npm/React 扩展配置，而非把包解析与转译规则固化为核心必需能力。该配置从调用方已安装的 `node_modules` 读取依赖，不安装、更新或锁定包，也不承诺构建可复现；不执行依赖安装或构建脚本。官方配置固定其自身支持的包入口选择策略与工具版本，但包版本由调用方管理。

扩展按模块将 TypeScript 去类型、JSX 转换（React JSX runtime 或其他配置）和 CommonJS 转成核心可链接的 ESM；转换器输出的 ESM 导入继续进入同一模块图。官方 React 配置以生产包为目标，提供常见 `exports`/`require`、属性访问和循环依赖所需的实用 CJS 互操作，不承诺完整 Node.js 模块语义；无法静态确定目标的动态 `require` 在构建阶段报错。条件导出按配置选择，不因宿主环境意外选择开发或服务器入口。入口显式导出仍是宿主公开 API，依赖内部导出不自动暴露；VM 不加载其他 JS 模块。转换不得破坏 ESM 活绑定及所覆盖的 CJS 初始化行为。

资源扩展可把资源导入登记到 Build 的资源表，资源以规范化 ID 去重，内容及其脚本映射参与构建身份；资源不进入 VM 堆，也不由 VM 自动应用。宿主读取 Build 资源并自行决定消费方式。CSS 支持作为 entry 可达图中的副作用资源导入；CSS Modules 配置可同时生成供脚本导入的类名映射模块，以及供宿主消费的改写 CSS 资源。项目不实现样式应用。编译错误应关联原始模块 ID、源码位置与转换阶段；源码映射为可选诊断旁路，不参与执行或构建身份。扩展配置、loader 顺序和具体 API 形状留待实施阶段确定。
