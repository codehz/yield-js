// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  const obj = { a: 1, "[object Object]": undefined };
  const arr = [1, 2];
  arr.extra = undefined;
  expect(obj.hasOwnProperty("a")).toBe(true);
  expect(obj.hasOwnProperty("b")).toBe(false);
  expect(obj.hasOwnProperty("hasOwnProperty")).toBe(false);
  expect(obj.hasOwnProperty({})).toBe(true);
  expect(arr.hasOwnProperty("1")).toBe(true);
  expect(arr.hasOwnProperty("2")).toBe(false);
  expect(arr.hasOwnProperty("length")).toBe(true);
  expect(arr.hasOwnProperty("extra")).toBe(true);
  expect(arr.hasOwnProperty).toBe(obj.hasOwnProperty);
}
