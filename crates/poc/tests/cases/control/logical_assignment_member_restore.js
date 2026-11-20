// @pending ["wait:rhs"]
// @restore
// @fulfill ["wait:rhs", 7]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  let original = { v: null };
  let current = original;
  let calls = 0;
  function base() {
    calls += 1;
    return current;
  }
  function key() {
    calls += 1;
    return "v";
  }
  let result = (base()[key()] ??= (current = { v: 9 }, await wait("rhs")));
  expect(calls).toBe(2);
  expect(result).toBe(7);
  expect(original.v).toBe(7);
  expect(current.v).toBe(9);
}
