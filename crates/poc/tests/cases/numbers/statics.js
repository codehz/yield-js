// @returns undefined

import { expect } from 'test';

export function run() {
  expect(Number.EPSILON).toBe(2.220446049250313e-16);
  expect(Number.MAX_VALUE).toBe(1.7976931348623157e308);
  expect(Number.MIN_VALUE).toBe(5e-324);
  expect(Number.MIN_VALUE <= 0).toBe(false);
  expect(Number.MAX_SAFE_INTEGER).toBe(9007199254740991);
  expect(Number.MIN_SAFE_INTEGER).toBe(-9007199254740991);
  expect(Number.POSITIVE_INFINITY).toBe(Infinity);
  expect(Number.NEGATIVE_INFINITY !== -Infinity).toBe(false);
  expect(Object.is(Number.NaN, NaN)).toBe(true);
  expect(Number.isFinite(1.5)).toBe(true);
  expect(Number.isFinite(Infinity)).toBe(false);
  expect(Number.isFinite(-Infinity)).toBe(false);
  expect(Number.isFinite(NaN)).toBe(false);
  expect(Number.isInteger(-0)).toBe(true);
  expect(Number.isInteger(9007199254740992)).toBe(true);
  expect(Number.isInteger(1.5)).toBe(false);
  expect(Number.isInteger(Infinity)).toBe(false);
  expect(Number.isSafeInteger(Number.MAX_SAFE_INTEGER)).toBe(true);
  expect(Number.isSafeInteger(Number.MIN_SAFE_INTEGER)).toBe(true);
  expect(Number.isSafeInteger(9007199254740992)).toBe(false);
  expect(Number.isSafeInteger(-9007199254740992)).toBe(false);
  expect(Number.isSafeInteger(1.5)).toBe(false);
  expect(Number.isNaN(NaN)).toBe(true);
  expect(Number.isNaN(Infinity)).toBe(false);
  expect(Number.isNaN(0)).toBe(false);
  let nonNumber = {valueOf() { throw "must not coerce"; }};
  expect(Number.isFinite("1")).toBe(false);
  expect(Number.isFinite(null)).toBe(false);
  expect(Number.isFinite(nonNumber)).toBe(false);
  expect(Number.isInteger("1")).toBe(false);
  expect(Number.isInteger(nonNumber)).toBe(false);
  expect(Number.isSafeInteger("1")).toBe(false);
  expect(Number.isSafeInteger(nonNumber)).toBe(false);
  expect(Number.isNaN("NaN")).toBe(false);
  expect(Number.isNaN(undefined)).toBe(false);
  expect(Number.isNaN(nonNumber)).toBe(false);
}
