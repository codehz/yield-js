// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  const obj = { n: 100, calc(a, b) { return this.n + 10 * a + b; } };
  const other = { n: 3 };
  expect(obj.calc.call(other, 2, 4)).toBe(27);
  expect(obj.calc.apply(other, [4, 2])).toBe(45);
  expect(noArgs.call(undefined)).toBe(true);
  expect(noArgs.call()).toBe(true);
  expect(noArgs.apply(undefined, null)).toBe(true);
  expect(noArgs.apply()).toBe(true);
}

function noArgs(a) {
  return this === undefined && a === undefined;
}
