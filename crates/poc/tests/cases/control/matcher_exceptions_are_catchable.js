// @returns true

import { assert, expect } from 'test';

export function run() {
  let matcherCaught = false;
  let assertionCaught = false;
  try {
    expect(3).toBe(4);
  } catch (error) {
    matcherCaught = typeof error === 'string' && error.includes('toBe');
  }
  try {
    assert(false, 'checkpoint');
  } catch (error) {
    assertionCaught = typeof error === 'string' && error.includes('checkpoint');
  }
  return matcherCaught && assertionCaught;
}
