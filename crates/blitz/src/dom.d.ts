declare module "yield:dom" {
    export function Node(): unknown;
    export function Document(): unknown;
    export function Element(): unknown;
    export function HTMLElement(): unknown;
    export function HTMLButtonElement(): unknown;
    export function HTMLLabelElement(): unknown;
    export function HTMLUListElement(): unknown;
    export function HTMLLIElement(): unknown;
    export function HTMLIFrameElement(): unknown;
    export function HTMLInputElement(): unknown;
    export function openDocument(window: unknown): unknown;
    export function insertCss(uri: string): void;
    export function dispatchUserEvent(): boolean;
}
