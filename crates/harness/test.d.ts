// Public host modules of the test harness. Declared value names are the ones
// the Rust bindings register; scripts may import any subset.

declare module "test" {
    /** Starts a timer that calls `cb` after `ms` milliseconds; returns a task handle. */
    export function spawnDelayedTask(cb: unknown, ms: unknown): unknown;
    /** Cancels a timer handle returned by `spawnDelayedTask`. */
    export function cancelDelayedTask(handle?: unknown): void;
    /** Resolves after `ms` milliseconds. */
    export function sleep(ms: unknown): Promise<unknown>;
    /** Collects unreachable heap state without draining microtasks. */
    export function gc(): Promise<unknown>;
    /** Records a host inspection call. */
    export function inspect(value?: unknown): void;
    /** Returns the fixture seed value. */
    export function seed(): number;
    /** Starts a resumable wait operation labelled by `label`. */
    export function wait(label?: unknown): Promise<unknown>;
    /** Starts a pausable operation labelled by `label`. */
    export function pause(label: unknown): Promise<unknown>;
    /** Starts an operation that rejects after restore. */
    export function rejectAfterRestore(label: unknown): Promise<unknown>;
    /** Starts an operation that reissues its request after restore. */
    export function reissue(label: unknown): Promise<unknown>;
    /** Allocates a host Node instance carrying `label`. */
    export function bind(label: unknown): unknown;
    /** Records a label write on a host node. */
    export function setLabel(node?: unknown, label?: unknown): void;
    /** Allocates a host Matcher instance for `actual`. */
    export function expect(actual?: unknown): unknown;
    /** Throws a host string error unless `actual` is truthy. */
    export function assert(actual?: unknown, message?: unknown): void;
    /** Declared host error subclass constructor. */
    export class ParseError extends Error {
        constructor(message?: unknown, options?: unknown);
    }
    export class ErrorBase extends Error {
        detail(): unknown;
    }
    export class ErrorDerived extends ErrorBase {
        constructor(detail: unknown);
    }
    export class HostDOMException extends DOMException {
        constructor(message?: unknown, name?: unknown);
    }
    export class HostTypeError extends TypeError {
        constructor(message?: unknown, options?: unknown);
    }
    /** Returns the intrinsic TypeError constructor, independent of the global binding. */
    export function getTypeErrorConstructor(): TypeErrorConstructor;
    /** Constructs and throws an intrinsic TypeError on the active stack. */
    export function throwTypeError(message?: unknown, options?: unknown): never;
    /** Allocates a ParseError instance from the host. */
    export function allocError(message?: unknown): unknown;
    /** Allocates and throws a ParseError on the active stack. */
    export function lazyThrow(): void;
}
