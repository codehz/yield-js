// @returns undefined

import { expect } from 'test';

function method() {
  return this === undefined;
}

export function run() {
  const call = method;
  expect(call()).toBe(true);
}
