declare module "errsA" {
    /** Allocates a ParseError instance from the host. */
    export function make(): unknown;
    /** Allocates an error class the host never declared. */
    export function bad(): unknown;
    /** Declared host error subclass constructor. */
    export class ParseError extends Error {
        constructor(message?: unknown, options?: unknown);
    }
}

declare module "errsB" {
    /** Allocates a ParseError instance from the host. */
    export function make(): unknown;
    /** Allocates an error class the host never declared. */
    export function bad(): unknown;
    /** Declared host error subclass constructor. */
    export class ParseError extends TypeError {
        constructor(message?: unknown, options?: unknown);
    }
}

declare module "errsDOM" {
    export class HostDOMException extends DOMException {
        constructor(message: unknown, name: unknown);
    }
}
