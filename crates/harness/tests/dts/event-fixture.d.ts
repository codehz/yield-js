declare module "event-fixture" {
  export function node(parent: unknown): EventTarget;
  export function dispatch(node: EventTarget, detail: unknown, bubbles: boolean): boolean;
}
