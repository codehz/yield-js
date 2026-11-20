// @returns undefined

import { expect } from 'test';

function Box() {}

export function run() {
  Box.prototype.x = 7;
  let first = new Box();
  let second = new Box();
  first.x = undefined;
  expect(first.x).toBe(undefined);
  expect(second.x).toBe(7);
  expect(first.missing).toBe(undefined);
  expect(second.missing).toBe(undefined);
}
