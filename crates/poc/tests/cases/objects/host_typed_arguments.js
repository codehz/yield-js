// @returns true

import { assert, expect, wait } from 'test';

export function run() {
  const matcher = expect();
  if (matcher.value !== undefined) return false;
  if (matcher.toBe(undefined) !== undefined) return false;

  const bare = matcher.toBe;
  try {
    bare(undefined);
    return false;
  } catch (error) {
    if (!(error instanceof TypeError)) return false;
  }
  try {
    matcher.toBe();
    return false;
  } catch (error) {
    if (!(error instanceof TypeError)) return false;
  }
  try {
    assert(false);
    return false;
  } catch (error) {
    if (error !== 'assert: expected truthy, actual false') return false;
  }
  try {
    assert(false, undefined);
    return false;
  } catch (error) {
    if (error !== 'assert: expected truthy, actual false: undefined') return false;
  }
  try {
    wait();
    return false;
  } catch (error) {
    return error instanceof TypeError;
  }
}
