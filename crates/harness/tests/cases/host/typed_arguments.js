// @returns undefined

import { assert, expect, lastArgument, restAt, wait } from 'test';

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

  expect(assert(true, 'unused', 'extra')).toBe(undefined);

  const obj = {};
  const array = [1, 2];
  expect(lastArgument.length).toBe(0);
  expect(lastArgument()).toBe(null);
  expect(lastArgument(1)).toBe(1);
  expect(lastArgument(1, 2, obj)).toBe(obj);
  expect(lastArgument(array)).toBe(array);

  expect(restAt.length).toBe(2);
  expect(restAt(0)).toBe(null);
  expect(restAt(-1)).toBe(null);
  expect(restAt(-1, undefined, 'tail')).toBe(undefined);
  expect(restAt(0, undefined, 'first', obj)).toBe('first');
  expect(restAt(1, undefined, 'first', obj)).toBe(obj);
  expect(restAt(2, undefined, 'first', obj)).toBe(null);

  let restError;
  try {
    restAt();
  } catch (error) {
    restError = error;
  }
  expect(restError instanceof TypeError).toBe(true);

  let waitError;
  try {
    wait();
  } catch (error) {
    waitError = error;
  }
  expect(waitError instanceof TypeError).toBe(true);
}
