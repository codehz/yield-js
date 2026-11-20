// @returns undefined

import { expect } from 'test';

function f(a) {
  a = 9;
  const original = arguments[0];
  arguments[0] = 7;
  return [arguments, original, a];
}

export function run() {
  const first = f(2);
  const second = f(4);
  first[0][0] = 11;
  expect(first[0] !== second[0]).toBe(true);
  expect(first[1]).toBe(2);
  expect(second[1]).toBe(4);
  expect(first[2]).toBe(9);
  expect(second[2]).toBe(9);
  expect(second[0][0]).toBe(7);
}
