declare module "stale" {
    export function stash(value: unknown): void;
    export function unstash(): unknown;
}
