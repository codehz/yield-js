// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", null]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  const before = globalThis;
  globalThis.marker = 7;
  await wait("a");
  expect(globalThis.marker).toBe(7);
  expect(globalThis).toBe(globalThis);
  expect(globalThis).toBe(before);
  expect(globalThis.globalThis).toBe(globalThis);
  expect(globalThis.Promise).toBe(Promise);
}
