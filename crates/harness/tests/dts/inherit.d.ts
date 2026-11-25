declare module "inherit" {
    export function wait(): Promise<unknown>;
    export function assert(value: unknown, message?: unknown): void;
    export function Base(): unknown;
    export function Derived(): unknown;
    export function readBase(value: unknown): unknown;
}
