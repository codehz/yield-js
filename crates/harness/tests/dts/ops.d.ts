// e2e strategy fixtures: one recovery strategy per export.

declare module "ops" {
    export function pause(): Promise<unknown>;
    export function resume(): Promise<unknown>;
    export function reissue(): Promise<unknown>;
    export function reject(): Promise<unknown>;
}
