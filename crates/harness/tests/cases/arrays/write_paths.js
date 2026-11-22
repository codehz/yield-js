// @returns undefined

import { expect } from 'test';
export function run() {
  const values = [1, 2];
  values[0] = 7;
  expect(values[0]).toBe(7);

  Object.defineProperty(values, "1", { writable: false });
  let frozenError;
  try { values[1] = 9; } catch (error) { frozenError = error; }
  expect(frozenError instanceof TypeError).toBe(true);
  expect(values[1]).toBe(2);

  let seen = 0;
  Object.defineProperty(values, "2", {
    configurable: true,
    set(value) { seen = value; },
  });
  values[2] = 11;
  expect(seen).toBe(11);

  const sparse = [];
  sparse[3] = 4;
  expect(sparse.length).toBe(4);
  expect(sparse[3]).toBe(4);

  const extensible = [1];
  Object.preventExtensions(extensible);
  extensible[0] = 5;
  expect(extensible[0]).toBe(5);
}
