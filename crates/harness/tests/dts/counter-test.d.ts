// e2e module-state fixture links counter and test together.

declare module "counter" {
    export function bump(): void;
    export function value(): number;
}

declare module "test" {
    export function wait(label: unknown): Promise<unknown>;
}
