declare module "repeat_timer" {
    export function start(callback: unknown): unknown;
    export function cancelDelayedTask(handle: unknown): void;
}
