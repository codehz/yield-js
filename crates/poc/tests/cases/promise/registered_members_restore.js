// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", null]
// @fulfilled 7

import { expect, wait } from 'test';

export async function run() {
  const is = Object.is;
  const resolve = Promise.resolve;
  await wait("a");

  expect(is(NaN, NaN)).toBe(true);
  let rejectedReceiver = false;
  try {
    resolve(7);
  } catch (error) {
    rejectedReceiver = error instanceof TypeError;
  }
  expect(rejectedReceiver).toBe(true);
  const value = await resolve.call(Promise, 7);
  expect(value).toBe(7);
  return value;
}
