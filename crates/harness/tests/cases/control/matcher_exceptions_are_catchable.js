// @returns undefined

import { assert, expect } from 'test';

export function run() {
  let matcherCaught = false;
  let assertionCaught = false;
  try {
    expect(3).toBe(4);
  } catch (error) {
    matcherCaught = Error.isError(error) && error.message.includes('toBe');
  }
  try {
    assert(false, 'checkpoint');
  } catch (error) {
    assertionCaught = Error.isError(error) && error.message.includes('checkpoint');
  }
  expect(matcherCaught).toBe(true);
  expect(assertionCaught).toBe(true);
}
