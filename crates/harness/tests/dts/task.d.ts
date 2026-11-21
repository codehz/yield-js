// e2e task ABI fixture: cancellable task handles.

declare module "task" {
    export function spawn(callback: unknown): unknown;
    export function cancel(handle: unknown): void;
}
