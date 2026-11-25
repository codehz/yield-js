# React + TSX 构建输入夹具

对齐 [docs/design/build.md](../../docs/design/build.md) 与 [framework-acceptance.md](../../docs/design/framework-acceptance.md) 的验收输入。`src/blitz.jsx` 用于真实 React DOM 首屏验收；`src/index.tsx`、类型剥离、CSS 与动态 sheet 不在本次首屏验收范围内。

## 覆盖点

| build.md 要求 | 夹具落点 |
| --- | --- |
| 应用入口直接使用 TSX | `src/index.tsx` |
| 静态导入解析到 `react/jsx-runtime` 的生产入口（`exports` 条件选择） | `src/index.tsx` 顶部导入 |
| 锁定版本 `react@19.2.0`、`react-dom@19.2.0`、`scheduler@0.27.0` | `package.json` 精确版本 |
| 函数组件、`useState`、effect 及清理、keyed 子节点重排、Promise 驱动 Suspense | `src/index.tsx` 的 `Counter` / `Reorder` / `App` |
| 转换产物作为模块进入同一 ESM 链接层 | `src/sheet.tsx`（由动态 import 到达） |
| CJS 静态可分析子集 + 预设条件选择 | `node_modules/scheduler/index.js`（`process.env.NODE_ENV` 分支 + 字面量 `require`，预设条件取 `cjs/scheduler.production.js`） |
| 无 `exports` 包的字段回退规则 | `scheduler`（无 `exports`，按预设定义回退到 `main`/`index.js`，不猜测其他字段） |
| CSS 副作用资源导入，按资源 ID 去重，宿主自行消费 | `src/styles.css` |
| React DOM 自带 reconciler 的首屏入口 | `src/blitz.jsx` |

## 约定

- 依赖由调用方预先安装到本目录 `node_modules`；官方 npm 预设只读取已安装依赖，不安装、不更新。
- `tsconfig.json` 仅用于开发期类型检查（`tsc --noEmit`）；构建期 TypeScript 转换只移除类型语法，不做类型检查。
- 组件使用 `react-dom` 的 DOM intrinsic elements；不声明非 Web 的 `IntrinsicElements`，也不提供自定义 `HostConfig`。
- CJS 依赖（`scheduler` 生产包为 CJS）用于验收 CJS 静态可分析子集转换；动态 `require` 应在构建期报错，本夹具不含此类用法的成功场景。
- 资源（CSS）不进入 VM 堆，由宿主从 Build 读取后自行消费。

## Blitz 首屏入口

`src/blitz.jsx` 使用 `react-dom/client` 的 `createRoot` 和 `react-dom` 的 `flushSync`。调用方从仓库根目录准备锁定依赖并验收：

```sh
npm ci --ignore-scripts --prefix fixtures/npm-react-app
cargo test -p yield-blitz --test render mounts_react_dom_into_blitz
```

验收检查 `#root > main > h1` 的文本为 `Hello from React DOM`；缺少 `node_modules` 时测试失败并提示准备命令。
