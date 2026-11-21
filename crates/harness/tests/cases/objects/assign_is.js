// @returns undefined

import { expect } from 'test';

export function run() {
  const target = {};
  const source = { get value() { return 3; } };
  const symbol = Symbol("key");
  source[symbol] = 4;
  const assigned = Object.assign(target, null, source);
  expect(assigned).toBe(target);
  expect(target.value).toBe(3);
  expect(target[symbol]).toBe(4);
  expect(Object.is(0 / 0, 0 / 0)).toBe(true);
  expect(Object.is(0, -0)).toBe(false);
  expect(Object.is(-0, -0)).toBe(true);
  expect(Object.is({}, {})).toBe(false);
  expect(Object.is("x", "x")).toBe(true);
}
