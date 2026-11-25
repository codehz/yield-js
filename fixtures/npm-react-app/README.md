# React + TSX 构建输入夹具

对齐 [docs/design/build.md](../../docs/design/build.md) 与 [framework-acceptance.md](../../docs/design/framework-acceptance.md) 的验收输入。本目录是测试素材（fixture），不是可运行示例：当前 compiler 尚不支持 TSX 与 npm 解析（见 docs/gaps.md），待 compiler 侧实现后由此验收。

## 覆盖点

| build.md 要求 | 夹具落点 |
| --- | --- |
| 应用入口直接使用 TSX | `src/index.tsx` |
| 静态导入解析到 `react/jsx-runtime` 的生产入口（`exports` 条件选择） | `src/index.tsx` 顶部导入 |
| 锁定版本 `react@19.2.0`、`react-reconciler@0.33.0`、`scheduler@0.27.0` | `package.json` 精确版本 |
| mutation-mode `HostConfig`、可观察内存节点树 | `src/host-config.ts` |
| 函数组件、`useState`、effect 及清理、keyed 子节点重排、Promise 驱动 Suspense | `src/index.tsx` 的 `Counter` / `Reorder` / `App` |
| 转换产物作为模块进入同一 ESM 链接层 | `src/sheet.tsx`（由动态 import 到达） |
| CJS 静态可分析子集 + 预设条件选择 | `node_modules/scheduler/index.js`（`process.env.NODE_ENV` 分支 + 字面量 `require`，预设条件取 `cjs/scheduler.production.js`） |
| 无 `exports` 包的字段回退规则 | `scheduler`、`react-reconciler`（无 `exports`，按预设定义回退到 `main`/`index.js`，不猜测其他字段） |
| CSS 副作用资源导入，按资源 ID 去重，宿主自行消费 | `src/styles.css` |

## 约定

- 依赖由调用方预先安装到本目录 `node_modules`；官方 npm 预设只读取已安装依赖，不安装、不更新。
- `tsconfig.json` 仅用于开发期类型检查（`tsc --noEmit`）；构建期 TypeScript 转换只移除类型语法，不做类型检查。
- `src/host-elements.d.ts` 为非 DOM 宿主声明 `IntrinsicElements`，元素名与 `host-config.ts` 的节点树对应。
- CJS 依赖（`scheduler` 生产包为 CJS）用于验收 CJS 静态可分析子集转换；动态 `require` 应在构建期报错，本夹具不含此类用法的成功场景。
- 资源（CSS）不进入 VM 堆；`roots` 数组是宿主消费的节点树观察点。

## 使用方式（compiler 实现后）

```sh
npm install --prefix fixtures/npm-react-app   # 调用方负责安装依赖
cargo run -p yield-harness …                  # 以 src/index.tsx 为 entry 构建并执行
```

预期：构建产出可链接 ESM 字节码；执行后 `roots` 中出现 `app` 节点树；同一构建产物在参考 JS 引擎（ESM 模式 + 同一 HostConfig）下节点树与事件日志一致。

## Blitz 首屏入口

`src/blitz.jsx` 是独立于上述 reconciler 验收入口的 `react-dom/client` 首次挂载用例。已生成的 `blitz.mjs` 是 Rust crate 的签入输入；消费者不需要 Node.js 或 `node_modules`。维护 fixture 时在本目录运行：

```sh
npm ci
npx --no-install esbuild src/blitz.jsx --bundle --format=esm --platform=browser --define:process.env.NODE_ENV='"production"' --jsx=automatic --legal-comments=inline --outfile=blitz.mjs
```
