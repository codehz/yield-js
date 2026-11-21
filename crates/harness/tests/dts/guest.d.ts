declare module "guest" {
    export function later(callee: unknown): unknown;
    export function stash(callee: unknown): void;
    export function stashGc(): void;
    export function replay(): unknown;
    export function fault(): void;
    export function mark(): void;
    export function twice(callee: unknown, seed: unknown): unknown;
    export function catchThenCompare(original: unknown, throwing: unknown, returning: unknown): unknown;
}
