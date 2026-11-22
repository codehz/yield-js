// @returns undefined

import { expect } from 'test';

export function run() {
  const sum = Math.sumPrecise;
  expect(sum.name).toBe('sumPrecise');
  expect(sum.length).toBe(1);
  expect(Object.is(sum([]), -0)).toBe(true);
  expect(Object.is(sum([-0, -0]), -0)).toBe(true);
  expect(Object.is(sum([-0, 0]), 0)).toBe(true);
  expect(Object.is(sum([-3, 3, -0]), 0)).toBe(true);
  expect(sum([1e20, 1, -1e20])).toBe(1);
  expect(sum(new Set([1e20, 1, -1e20]))).toBe(1);

  const tiny = Number.MIN_VALUE;
  const ulp = Number.EPSILON;
  expect(sum([tiny, tiny, -tiny])).toBe(tiny);
  expect(sum([2 ** -1022, -tiny])).toBe(2 ** -1022 - tiny);
  expect(sum([1, ulp / 2])).toBe(1);
  expect(sum([1, ulp, ulp / 2])).toBe(1 + 2 * ulp);
  expect(sum([1, ulp / 2, tiny])).toBe(1 + ulp);
  expect(sum([1, ulp / 2, -tiny])).toBe(1);
  expect(sum([-1, -ulp / 2, -tiny])).toBe(-1 - ulp);
  expect(sum([2, -ulp / 2])).toBe(2);

  const max = Number.MAX_VALUE;
  expect(sum([max, max, -max])).toBe(max);
  expect(sum([-max, -max, max])).toBe(-max);
  expect(sum([max, max, -max, -max, tiny])).toBe(tiny);
  expect(sum([max, max])).toBe(Infinity);
  expect(sum([-max, -max])).toBe(-Infinity);
  expect(sum([max, 2 ** 970])).toBe(Infinity);
  expect(sum([max, 2 ** 970, -tiny])).toBe(max);
  expect(sum([max, max, -Infinity])).toBe(-Infinity);
  expect(sum([Infinity, 1, Infinity])).toBe(Infinity);
  expect(Number.isNaN(sum([Infinity, -Infinity]))).toBe(true);
  expect(Number.isNaN(sum([Infinity, NaN]))).toBe(true);
}
