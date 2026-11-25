// 对齐 docs/design/framework-acceptance.md：mutation-mode HostConfig，
// 用可观察的内存节点树验收。类型随 TSX 转换移除，不影响运行语义。
export type Instance = {
  type: string;
  props: Record<string, unknown>;
  children: Instance[];
  parent: Instance | null;
  root: Instance;
  text: string;
};

export const roots: Instance[] = [];
let uid = 0;

function createInstance(
  type: string,
  props: Record<string, unknown>,
  root: Instance,
): Instance {
  uid += 1;
  return { type: `${type}#${uid}`, props, children: [], parent: null, text: "", root };
}

export function appendChild(parent: Instance, child: Instance): void {
  child.parent = parent;
  parent.children.push(child);
}

export const hostConfig = {
  isPrimaryRenderer: true,
  supportsMutation: true,
  supportsHydration: false,
  supportsPersistence: false,

  createInstance(
    type: string,
    props: Record<string, unknown>,
    root: Instance,
  ): Instance {
    return createInstance(type, props, root);
  },

  createTextInstance(
    text: string,
    root: Instance,
  ): Instance {
    const node = createInstance("#text", {}, root);
    node.text = text;
    return node;
  },

  appendInitialChild(parent: Instance, child: Instance): void {
    appendChild(parent, child);
  },

  appendChild(parent: Instance, child: Instance): void {
    appendChild(parent, child);
  },

  appendChildToContainer(container: Instance, child: Instance): void {
    appendChild(container, child);
    roots.push(container);
  },

  removeChild(parent: Instance, child: Instance): void {
    const index = parent.children.indexOf(child);
    if (index >= 0) {
      parent.children.splice(index, 1);
    }
    child.parent = null;
  },

  insertBefore(
    parent: Instance,
    child: Instance,
    beforeChild: Instance,
  ): void {
    const index = parent.children.indexOf(beforeChild);
    parent.children.splice(index < 0 ? parent.children.length : index, 0, child);
    child.parent = parent;
  },

  prepareUpdate(): null {
    return null;
  },

  commitUpdate(instance: Instance, props: Record<string, unknown>): void {
    instance.props = props;
  },

  commitTextUpdate(instance: Instance, text: string): void {
    instance.text = text;
  },

  clearContainer(container: Instance): void {
    container.children.length = 0;
  },

  getRootHostConfig(): { rootInstance: Instance } | Record<string, never> {
    return {};
  },

  shouldSetTextContent(): boolean {
    return false;
  },

  getChildHostContext(): Record<string, never> {
    return {};
  },

  finalizeInitialChildren(): boolean {
    return false;
  },

  prepareForCommit(): null {
    return null;
  },

  resetAfterCommit(): void {},

  now(): number {
    return Date.now();
  },

  scheduleTimeout(fn: () => void, delay: number): number {
    // 宿主须提供可取消 timer；此处仅标记宿主 API 边界，真实实现在宿主绑定层。
    throw new Error("scheduleTimeout must be provided by the host binding");
  },

  cancelTimeout(id: number): void {
    throw new Error("cancelTimeout must be provided by the host binding");
  },

  getCurrentEventPriority(): number {
    return 1;
  },
};
