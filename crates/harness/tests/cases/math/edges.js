// @returns undefined

import { expect } from 'test';

export function run() {
  expect(Math.max()).toBe(-Infinity);
  expect(Math.min()).toBe(Infinity);
  expect(Math.hypot() === 0).toBe(true);
  expect(Math.max(1, 3, 2)).toBe(3);
  expect(Math.min(1, -3, -2)).toBe(-3);
  expect(Object.is(Math.max(-0, 0, -0), 0)).toBe(true);
  expect(Object.is(Math.max(-0, -0), -0)).toBe(true);
  expect(Object.is(Math.min(0, -0, 0), -0)).toBe(true);
  expect(Object.is(Math.min(0, 0), 0)).toBe(true);
  expect(Number.isNaN(Math.max(NaN, Infinity))).toBe(true);
  expect(Number.isNaN(Math.min(-Infinity, NaN))).toBe(true);
  expect(Math.hypot(3, -4, 12)).toBe(13);
  expect(Math.hypot(NaN, -Infinity)).toBe(Infinity);
  expect(Number.isNaN(Math.hypot(Number.MAX_VALUE, Number.MAX_VALUE, NaN))).toBe(true);
  expect(Math.hypot(1e300, 1e300) > 1e300).toBe(true);
  expect(Math.hypot(1e-300, 1e-300) > 1e-300).toBe(true);
  expect(Math.round(1.5)).toBe(2);
  expect(Math.round(-1.5)).toBe(-1);
  expect(Math.round(0.49999999999999994) === 0).toBe(true);
  expect(Object.is(Math.round(-0.5), -0)).toBe(true);
  expect(Object.is(Math.round(-0.1), -0)).toBe(true);
  expect(Object.is(Math.round(-0), -0)).toBe(true);
  expect(Math.round(Infinity)).toBe(Infinity);
  expect(Number.isNaN(Math.round())).toBe(true);
  expect(Object.is(Math.ceil(-0.5), -0)).toBe(true);
  expect(Object.is(Math.sign(-0), -0)).toBe(true);
  expect(Object.is(Math.abs(-0), 0)).toBe(true);
  expect(Object.is(Math.fround(-1e-50), -0)).toBe(true);
  expect(Object.is(Math.sqrt(-0), -0)).toBe(true);
  expect(Number.isNaN(Math.sqrt(-1))).toBe(true);
  expect(Math.log(0)).toBe(-Infinity);
  expect(Object.is(Math.atan2(-0, 0), -0)).toBe(true);
  expect(Math.atan2(-0, -0)).toBe(-Math.PI);
  expect(Math.atan2(Infinity, Infinity)).toBe(Math.PI / 4);
  expect(Math.pow(NaN, 0)).toBe(1);
  expect(Number.isNaN(Math.pow(1, NaN))).toBe(true);
  expect(Number.isNaN(Math.pow(-1, Infinity))).toBe(true);
  expect(Object.is(Math.pow(-0, 3), -0)).toBe(true);
  expect(Math.pow(-0, -3)).toBe(-Infinity);
}
