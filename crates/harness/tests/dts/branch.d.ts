declare module "branch" {
    export function choose(callee: unknown, mode: unknown): unknown;
    export function spawn(callee: unknown, seed: unknown): unknown;
    export function repeat(callee: unknown): unknown;
    export function countWhile(callee: unknown): unknown;
    export function countWhileLet(callee: unknown): unknown;
    export function labeled(callee: unknown): unknown;
    export function nativeFor(callee: unknown): unknown;
    export function selectorJump(callee: unknown): unknown;
    export function cancel(handle: unknown): void;
    export function early(mode: unknown): unknown;
}

declare module "mirror" {
    export function choose(): unknown;
}
