// @pending ["wait:it"]
// @restore
// @fulfill ["wait:it", 6]
// @fulfilled true

// Upstream: streams/readable-streams/from.any.js
import { wait } from "test";

export async function run() {
  const source = {
    i: 0,
    [Symbol.asyncIterator]() { return this; },
    async next() {
      this.i++;
      if (this.i === 1) return { value: await wait("it"), done: false };
      return { value: undefined, done: true };
    }
  };
  const values = [];
  for await (const value of ReadableStream.from(source)) values.push(value);
  return values.join(",") === "6";
}
