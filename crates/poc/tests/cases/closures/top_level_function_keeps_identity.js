// @returns undefined

import { expect } from 'test';

function demo() {
  return 1;
}

export function run() {
  const alias = demo;
  expect(alias).toBe(demo);
}
