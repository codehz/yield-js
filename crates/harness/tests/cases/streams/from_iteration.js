// @fulfilled true

// Upstream: streams/readable-streams/from.any.js and streams/readable-streams/async-iterator.any.js
export async function run() {
  let returned;
  const iterable = {
    i: 0,
    [Symbol.asyncIterator]() { return this; },
    async next() { return this.i < 2 ? { value: ++this.i, done: false } : { value: undefined, done: true }; },
    async return(value) { returned = value; return { value, done: true }; }
  };
  const stream = ReadableStream.from(iterable);
  const reader = stream.getReader();
  const first = (await reader.read()).value;
  reader.releaseLock();
  const values = [];
  for await (const value of stream) { values.push(value); }
  const early = ReadableStream.from({ [Symbol.iterator]: function* () { yield 1; yield 2; } });
  const iterator = early.values();
  if ((await iterator.next()).value !== 1) return false;
  const done = await iterator.return();
  return first === 1 && values.join(",") === "2" && done.done && !early.locked && returned === undefined
    && iterator[Symbol.asyncIterator]() === iterator;
}
