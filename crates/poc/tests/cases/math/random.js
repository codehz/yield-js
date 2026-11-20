// @returns undefined

import { expect } from 'test';

export function run() {
  const ignored = { valueOf() { throw 'random ignores arguments'; } };
  let previous = Math.random(ignored);
  expect(previous >= 0 && previous < 1).toBe(true);
  for (let i = 0; i < 32; i++) {
    const next = Math.random();
    expect(next >= 0 && next < 1).toBe(true);
    expect(next === previous).toBe(false);
    previous = next;
  }
}
