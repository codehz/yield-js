import { wait } from "test";

function* syncSequence() {
    try {
        try {
            const sent = yield 10;
            yield sent + 1;
        } catch (reason) {
            yield reason + 2;
        }
    } finally {
        yield 40;
    }
}

async function* asyncSequence() {
    try {
        try {
            const sent = yield 10;
            yield sent + 1;
        } catch (reason) {
            yield reason + 2;
        }
    } finally {
        yield 40;
    }
}

function matches(result, value, done) {
    return result.value === value && result.done === done;
}

export async function sync() {
    const iterator = syncSequence();
    if (!matches(iterator.next(), 10, false)) return -1;
    const input = await wait("baseline-generator");
    if (!matches(iterator.next(input), 21, false)) return -2;
    if (!matches(iterator.throw(30), 32, false)) return -3;
    if (!matches(iterator.return(50), 40, false)) return -4;
    if (!matches(iterator.next(), 50, true)) return -5;
    return matches(iterator.next(), undefined, true) ? 1 : -6;
}

export async function asyncGenerator() {
    const iterator = asyncSequence();
    if (!matches(await iterator.next(), 10, false)) return -1;
    const input = await wait("baseline-generator");
    if (!matches(await iterator.next(input), 21, false)) return -2;
    if (!matches(await iterator.throw(30), 32, false)) return -3;
    if (!matches(await iterator.return(50), 40, false)) return -4;
    if (!matches(await iterator.next(), 50, true)) return -5;
    return matches(await iterator.next(), undefined, true) ? 1 : -6;
}
