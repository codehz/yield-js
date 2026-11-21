// @pending ["wait:close"]
// @restore
// @fulfill ["wait:close", "x"]
// @fulfilled "1|close:1|after"
import { wait } from 'test';

export async function run() {
  const trace = [];
  let closes = 0;
  const iterable = {
    [Symbol.asyncIterator]() { return this; },
    next() { return Promise.resolve({ value: 1, done: false }); },
    return() {
      closes += 1;
      return wait("close").then(() => ({ done: true }));
    },
  };
  for await (const value of iterable) {
    trace.push(value);
    break;
  }
  trace.push(`close:${closes}`);
  trace.push("after");
  return trace.join("|");
}
