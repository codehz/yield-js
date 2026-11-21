// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a",0]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  const matcher = expect(1);
  const prototype = Object.getPrototypeOf(matcher);
  prototype.toBe = function () { return 'patched'; };
  delete prototype.toEqual;
  await wait('a');
  expect(matcher.toBe(2) === 'patched').toBeTruthy();
  expect(typeof matcher.toEqual === 'undefined').toBeTruthy();
  expect(Object.getPrototypeOf(matcher) === prototype).toBeTruthy();
}
