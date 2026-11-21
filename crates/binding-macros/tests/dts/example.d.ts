// Host modules linked by tests/external.rs.

declare module "example" {
    /** Allocates an Example instance. */
    export function Example(item?: unknown): unknown;
    export const version: number;
}
