// @pending ["wait:d"]
// @restore
// @fulfill ["wait:d", 5]
// @fulfilled "1|5:false"

import { wait } from 'test';

export async function run() {
  let calls = 0;
  const delegate = {
    [Symbol.asyncIterator]() {
      return this;
    },
    next() {
      calls = calls + 1;
      return wait("d").then((value) => ({ value, done: false }));
    },
  };
  async function* g() {
    yield* delegate;
  }
  const it = g();
  // The delegate step is suspended inside its own await when the snapshot is
  // taken, so a restored run must not call `next` a second time.
  const first = it.next();
  const settled = await first;
  return `${calls}|${settled.value}:${settled.done}`;
}
