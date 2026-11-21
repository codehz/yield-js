// @returns undefined

import { expect } from 'test';

export function run() {
  let leading = [, 7, 8];
  let trailing = [7, 8, ];
  let multi = [1, , , 4];
  expect(leading.length).toBe(3);
  expect(0 in leading).toBe(false);
  expect(leading[0]).toBe(undefined);
  expect(trailing.length).toBe(2);
  expect(2 in trailing).toBe(false);
  expect(multi.length).toBe(4);
  expect(1 in multi).toBe(false);
  expect(2 in multi).toBe(false);
  expect(Array(3).length).toBe(3);
  expect(0 in Array(3)).toBe(false);
  expect([...multi][3]).toBe(4);
  let spread = [...multi];
  expect(spread.length).toBe(4);
  expect(1 in spread).toBe(true);
  expect(spread[1]).toBe(undefined);
}
