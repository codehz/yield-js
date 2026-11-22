// @returns undefined

import { expect } from 'test';

export function run() {
  const round = Math.f16round;
  expect(round.name).toBe('f16round');
  expect(round.length).toBe(1);
  expect(Number.isNaN(round())).toBe(true);
  expect(round(-Infinity)).toBe(-Infinity);
  expect(Object.is(round(-0), -0)).toBe(true);
  expect(round('1.5')).toBe(1.5);
  expect(round(1 + 2 ** -11)).toBe(1);
  expect(round(1 + 2 ** -11 + 2 ** -52)).toBe(1 + 2 ** -10);
  expect(round(1 + 3 * 2 ** -11)).toBe(1 + 2 ** -9);
  expect(round(2 ** -24)).toBe(2 ** -24);
  expect(Object.is(round(-(2 ** -25)), -0)).toBe(true);
  expect(Object.is(round(-(2 ** -36)), -0)).toBe(true);
  expect(round(3 * 2 ** -25)).toBe(2 ** -23);
  expect(round(65519)).toBe(65504);
  expect(round(65520)).toBe(Infinity);
  let coerced = 0;
  expect(round({ valueOf() { coerced++; return 1; } })).toBe(1);
  expect(coerced).toBe(1);
}
