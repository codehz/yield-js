// @returns undefined

import { expect } from 'test';

export function run() {
  let order = '';
  const marker = {};
  const a = { valueOf() { order += 'a'; return '2'; } };
  const b = { valueOf() { order += 'b'; return 3; } };
  const bad = { valueOf() { order += 'x'; throw marker; } };
  expect(Math.pow(a, b, bad)).toBe(8);
  expect(order).toBe('ab');
  expect(Number.isNaN(Math.max(NaN, b, a))).toBe(true);
  expect(order).toBe('abba');
  let hypotError;
  try {
    Math.hypot(Infinity, a, bad, b);
  } catch (error) {
    hypotError = error;
  }
  expect(hypotError).toBe(marker);
  expect(order).toBe('abbaax');

  let imulError;
  try {
    Math.imul(bad, b);
  } catch (error) {
    imulError = error;
  }
  expect(imulError).toBe(marker);
  expect(order).toBe('abbaaxx');

  let absError;
  try {
    Math.abs(Symbol());
  } catch (error) {
    absError = error;
  }
  expect(absError instanceof TypeError).toBe(true);

  let minError;
  try {
    Math.min(NaN, Symbol());
  } catch (error) {
    minError = error;
  }
  expect(minError instanceof TypeError).toBe(true);
}
