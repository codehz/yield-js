// @pending ["wait:😀"]
// @restore
// @fulfill ["wait:😀", "ok"]
// @pending ["wait:done"]
// @restore
// @fulfill ["wait:done", true]
// @fulfilled "é|😀|ok:汉|:done"

import { wait } from 'test';

export async function run() {
  const iterator = "é😀汉"[Symbol.iterator]();
  let trace = `${iterator.next().value}|`;
  for (const ch of iterator) {
    trace += ch + "|";
    if (ch === "😀") trace += (await wait(ch)) + ":";
  }
  await wait("done");
  return trace + (iterator.next().done ? ":done" : ":not done");
}
