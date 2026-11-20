// @pending ["wait:rhs"]
// @restore
// @fulfill ["wait:rhs", 7]
// @fulfilled true

import { wait } from 'test';

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
  return calls === 2 && result === 7 && original.v === 7 && current.v === 9;
}
