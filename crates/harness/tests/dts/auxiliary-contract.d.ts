// e2e signature fingerprint fixtures link auxiliary and contract together.

declare module "auxiliary" {
}

declare module "contract" {
    export function probe(value: unknown): unknown;
    export function wait(): Promise<unknown>;
}
