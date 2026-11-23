// e2e strategy fixtures: one recovery strategy per export.

declare module "ops" {
    export function pause(): Promise<unknown>;
    export function resume(): Promise<unknown>;
    export function withSignal(signal: AbortSignal): Promise<unknown>;
    export function abortReason(signal: AbortSignal): unknown;
    export function reissue(): Promise<unknown>;
    export function reject(): Promise<unknown>;
}
