// @pending ["wait:body"]
// @restore
// @fulfill ["wait:body", 0]
// @pending ["wait:step"]
// @restore
// @fulfill ["wait:step", 2]
// @fulfilled "1|2|3|true"
import { wait } from 'test';

export async function run() {
  let calls = 0;
  let closed = 0;
  const iterable = {
    [Symbol.asyncIterator]() { return this; },
    next() {
      calls += 1;
      if (calls === 1) return Promise.resolve({ value: 1, done: false });
      if (calls === 2) return wait("step").then((value) => ({ value, done: false }));
      return Promise.resolve({ done: true });
    },
    return() { closed += 1; return Promise.resolve({ done: true }); },
  };
  const values = [];
  for await (const value of iterable) {
    values.push(value);
    if (value === 1) await wait("body");
  }
  return `${values.join("|")}|${calls}|${closed === 0}`;
}
