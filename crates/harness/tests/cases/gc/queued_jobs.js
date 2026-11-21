// @pending ["wait:gc"]
// @restore
// @fulfill ["wait:gc", null]
// @fulfilled "ab42"

import { expect, gc, wait } from "test";

function queueReader(append) {
  const object = { value: 42 };
  queueMicrotask(() => { append("b" + object.value); });
}

export async function run() {
  await wait("gc");
  let log = "";
  queueMicrotask(() => {
    gc();
    log += "a";
  });
  queueReader(value => { log += value; });
  expect(log).toBe("");
  gc();
  expect(log).toBe("");
  await undefined;
  return log;
}
