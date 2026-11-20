// @pending ["wait:number"]
// @restore
// @fulfill ["wait:number", null]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  const constructor = Number;
  const prototype = Number.prototype;
  await wait("number");
  expect(constructor).toBe(Number);
  expect(prototype).toBe(Number.prototype);
  expect(prototype.constructor).toBe(Number);
  expect((1).valueOf).toBe(prototype.valueOf);
  expect(Number.isNaN(Number.NaN)).toBe(true);
  expect(Number.POSITIVE_INFINITY).toBe(Infinity);
  expect(Number.NEGATIVE_INFINITY === -Infinity).toBe(true);
}
