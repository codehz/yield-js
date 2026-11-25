// 非 Web 宿主的元素表：本夹具不用 DOM，宿主树元素由 HostConfig 定义。
// 与 src/host-config.ts 的内存节点树元素名对应。
import type { JSX, ReactNode } from "react";

declare module "react" {
  namespace JSX {
    interface IntrinsicElements {
      app: { children?: ReactNode };
      button: { onClick: () => void; children?: ReactNode };
      item: { children?: ReactNode; key?: string };
      sheet: { children?: ReactNode };
      loading: { children?: ReactNode };
    }
  }
}

export {};
