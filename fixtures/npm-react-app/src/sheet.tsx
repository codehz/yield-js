// 懒加载模块：由 index.tsx 的动态 import 在运行时加载，
// 依赖到达时进入模块图，验证转换产物进入同一 ESM 链接层。
import type { JSX } from "react";

export function Sheet(props: { names: string[] }): JSX.Element {
  return (
    <ul>
      {props.names.map((name) => (
        <li key={name}>{name}</li>
      ))}
    </ul>
  );
}
