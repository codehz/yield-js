// TSX 应用入口：验收 docs/design/build.md 的 TSX→JSX-runtime 转换与
// 静态 ESM 导入解析（react@19.2.0 的 exports 条件选择，React 预设取生产入口）。
import type { JSX } from "react";
import { useState, useEffect, Suspense, lazy } from "react";
import "./styles.css";

export function Button(props: { label: string; onClick: () => void }): JSX.Element {
  return <button onClick={props.onClick}>{props.label}</button>;
}

export function ItemList(props: { items: string[] }): JSX.Element {
  return (
    <ul>
      {props.items.map((item) => (
        <li key={item}>{item}</li>
      ))}
    </ul>
  );
}

export function Counter(): JSX.Element {
  const [count, setCount] = useState(0);

  useEffect(() => {
    // 带清理的 effect：验收副作用与 cleanup 不重跑、恢复后不重复挂载。
    return () => {
      setCount(-1);
    };
  }, []);

  return (
    <Button label={`count=${count}`} onClick={() => setCount(count + 1)} />
  );
}

// keyed 子节点重排：删除 + 保留项复用实例。
export function Reorder({ names }: { names: string[] }): JSX.Element {
  return <ItemList items={names} />;
}

const Sheet = lazy(async () => {
  // 故意用动态 import：React.lazy 需要延迟求值的 loader 制造 Promise 驱动的
  // Suspense 恢复切点（framework-acceptance 验收项）；静态导入会在模块图
  // 构建期就解析，无法产生该切点。模块路径本身是已知的字面量。
  const mod = await import("./sheet.js");
  return { default: mod.Sheet };
});

export function App(): JSX.Element {
  const names = ["alpha", "beta", "gamma"];
  return (
    <main>
      <Suspense fallback={<span />}>
        <Sheet names={names} />
      </Suspense>
    </main>
  );
}

export function render(
  mount: (element: JSX.Element) => void,
): void {
  mount(<App />);
}
