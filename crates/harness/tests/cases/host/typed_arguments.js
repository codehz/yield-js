// @returns undefined

import { assert, expect, wait } from 'test';

export function run() {
  const matcher = expect();
  expect(matcher.value).toBe(undefined);
  expect(matcher.toBe(undefined)).toBe(undefined);

  const bare = matcher.toBe;
  let bareError;
  try {
    bare(undefined);
  } catch (error) {
    bareError = error;
  }
  expect(bareError instanceof TypeError).toBe(true);

  let missingArgumentError;
  try {
    matcher.toBe();
  } catch (error) {
    missingArgumentError = error;
  }
  expect(missingArgumentError instanceof TypeError).toBe(true);

  let assertError;
  try {
    assert(false);
  } catch (error) {
    assertError = error;
  }
  expect(assertError instanceof Error).toBe(true);
  expect(assertError.message).toBe('assert: expected truthy, actual false');

  let explicitMessageError;
  try {
    assert(false, undefined);
  } catch (error) {
    explicitMessageError = error;
  }
  expect(explicitMessageError.message).toBe('assert: expected truthy, actual false: undefined');

  let waitError;
  try {
    wait();
  } catch (error) {
    waitError = error;
  }
  expect(waitError instanceof TypeError).toBe(true);
}
