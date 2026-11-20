// @returns undefined

import { expect } from 'test';

function near(x, y) { return Math.abs(x - y) < 1e-12; }
export function run() {
  expect(Math.abs('-2')).toBe(2);
  expect(Math.acos(1) === 0).toBe(true);
  expect(Math.acosh(1) === 0).toBe(true);
  expect(near(Math.asin(1), Math.PI / 2)).toBe(true);
  expect(near(Math.asinh(Math.sinh(1)), 1)).toBe(true);
  expect(near(Math.atan(1), Math.PI / 4)).toBe(true);
  expect(near(Math.atanh(Math.tanh(1)), 1)).toBe(true);
  expect(near(Math.atan2(1, -1), 3 * Math.PI / 4)).toBe(true);
  expect(Math.cbrt(-8)).toBe(-2);
  expect(Math.ceil(1.2)).toBe(2);
  expect(Math.floor(-1.2)).toBe(-2);
  expect(Math.trunc(-1.2)).toBe(-1);
  expect(Math.cos(0)).toBe(1);
  expect(near(Math.cosh(1), (Math.E + 1 / Math.E) / 2)).toBe(true);
  expect(near(Math.exp(1), Math.E)).toBe(true);
  expect(near(Math.expm1(1), Math.E - 1)).toBe(true);
  expect(near(Math.log(Math.E), 1)).toBe(true);
  expect(near(Math.log1p(Math.E - 1), 1)).toBe(true);
  expect(Math.log10(100)).toBe(2);
  expect(Math.log2(8)).toBe(3);
  expect(Math.pow(2, 8)).toBe(256);
  expect(Math.sign(-2)).toBe(-1);
  expect(Math.sign(Infinity)).toBe(1);
  expect(near(Math.sin(Math.PI / 2), 1)).toBe(true);
  expect(Math.sqrt(9)).toBe(3);
  expect(near(Math.tan(Math.PI / 4), 1)).toBe(true);
  expect(Math.fround(16777217)).toBe(16777216);
  expect(Math.fround(Number.MAX_VALUE)).toBe(Infinity);
  expect(Math.clz32()).toBe(32);
  expect(Math.clz32(4294967297)).toBe(31);
  expect(Math.clz32(-1) === 0).toBe(true);
  expect(Math.imul(4294967295, 5)).toBe(-5);
  expect(Math.imul(2147483648, 2) === 0).toBe(true);
  expect(Math.imul(Infinity, 5) === 0).toBe(true);
  expect(Math.imul(3.9, '-2')).toBe(-6);
}
