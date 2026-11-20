// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 7]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  let read = () => x;
  let before = read();
  let x = await wait("a");
  let after = read();
  expect(before).toBe(undefined);
  expect(after).toBe(7);
}
