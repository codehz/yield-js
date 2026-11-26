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
    export function HTMLDivElement(): unknown;
    export function HTMLSpanElement(): unknown;
    export function HTMLHeadingElement(): unknown;
    export function HTMLParagraphElement(): unknown;
    export function HTMLOListElement(): unknown;
    export function HTMLQuoteElement(): unknown;
    export function HTMLPreElement(): unknown;
    export function HTMLBRElement(): unknown;
    export function HTMLHRElement(): unknown;
    export function openDocument(window: unknown): unknown;
    export function insertCss(uri: string): void;
    export function dispatchUserEvent(): boolean;
}
