import { wait } from "test";

export async function success() {
    let total = await wait("baseline-await");
    for (let i = 0; i < 64; i++) total += await i;
    return total;
}

export async function rejectInFinally() {
    let trace = 0;
    async function inner() {
        try {
            trace = 1;
            return 99;
        } finally {
            trace = trace * 10 + 2;
            await wait("baseline-await");
            trace = -1; // Rejection must skip this and replace the pending return.
        }
    }
    try {
        await inner();
        return -2;
    } catch (reason) {
        return reason === "baseline rejection" ? trace : -3;
    }
}
