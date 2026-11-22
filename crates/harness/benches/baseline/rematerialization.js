import { wait } from "test";

export async function saved() {
    return 13 + (await wait("literal"));
}

export async function reloaded() {
    const input = await wait("literal");
    return 13 + input;
}
