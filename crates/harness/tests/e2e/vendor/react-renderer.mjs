import ReactReconciler from "react-reconciler-0.31.0.mjs";
import { clearTimeout, inspect, setTimeout } from "test";
import * as console from "console";


const hostConfig = {
  supportsMutation: true,
  createInstance(type, props) { return { type, props, children: [] }; },
  createTextInstance(text) { return { text }; },
  appendInitialChild(parent, child) { parent.children.push(child); },
  appendChild(parent, child) { parent.children.push(child); },
  appendChildToContainer(container, child) { container.children.push(child); },
  removeChild(parent, child) { parent.children.splice(parent.children.indexOf(child), 1); },
  removeChildFromContainer(container, child) { container.children.splice(container.children.indexOf(child), 1); },
  insertBefore(parent, child, before) { parent.children.splice(parent.children.indexOf(before), 0, child); },
  insertInContainerBefore(container, child, before) { container.children.splice(container.children.indexOf(before), 0, child); },
  commitUpdate(instance, type, oldProps, newProps) { instance.props = newProps; },
  commitTextUpdate(instance, oldText, newText) { instance.text = newText; },
  prepareUpdate() { return true; },
  finalizeInitialChildren() { return false; },
  shouldSetTextContent() { return false; },
  getRootHostContext() { return {}; },
  getChildHostContext() { return {}; },
  prepareForCommit() {},
  resetAfterCommit() {},
  clearContainer(container) { container.children.length = 0; },
  scheduleTimeout: setTimeout,
  cancelTimeout: clearTimeout,
  noTimeout: -1,
  supportsMicrotasks: false,
  getCurrentEventPriority() { return 16; },
};

export const renderer = ReactReconciler.default(hostConfig);
export function render(element, container) {
  const root = renderer.createContainer(container, 0, null, false, null, "headless", console.error, null);
  renderer.updateContainer(element, root, null, null);
  return container;
}
