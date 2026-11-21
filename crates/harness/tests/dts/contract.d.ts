// e2e timer fault fixtures link only the contract module.

declare module "contract" {
    export function schedule(callback: unknown, extra: unknown): unknown;
    export function other(): void;
}
