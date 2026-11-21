// Public host module of the demo binary.

declare module "demo" {
    /** Echoes its argument to the demo log. */
    export function inspect(value?: unknown): void;
    /** Returns the demo seed value. */
    export function seed(): number;
    /** Starts the resumable wait operation labelled by `label`. */
    export function wait(label: unknown): Promise<number>;
}
