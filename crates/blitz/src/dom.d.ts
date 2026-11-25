declare module "yield:dom" {
    export function byId(id: string): number | null;
    export function documentElement(): number | null;
    export function body(): number | null;
    export function element(tag: string): number;
    export function text(value: string): number;
    export function append(parent: number, child: number): void;
    export function setText(node: number, value: string): void;
    export function setAttr(node: number, name: string, value: string): void;
    export function first(node: number): number | null;
    export function next(node: number): number | null;
    export function parent(node: number): number | null;
    export function kind(node: number): number;
    export function name(node: number): string;
    export function readText(node: number): string | null;
}
