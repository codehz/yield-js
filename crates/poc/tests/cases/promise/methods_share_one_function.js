// @returns undefined

import { expect } from 'test';

export function run() {
  const left = Promise.resolve(1);
  const right = Promise.resolve(2);
  expect(left.then).toBe(right.then);
  expect(left.then).toBe(Promise.prototype.then);
}
