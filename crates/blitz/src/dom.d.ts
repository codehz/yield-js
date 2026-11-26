declare module "yield:dom" {
    export function Node(): unknown;
    export function Document(): unknown;
    export function Element(): unknown;
    export function HTMLIFrameElement(): unknown;
    export function HTMLInputElement(): unknown;
    export function openDocument(window: unknown): unknown;
    export function insertCss(uri: string): void;
    export function dispatchUserEvent(): boolean;
}
