// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a",2]
// @fulfilled 2

import { assert, expect, wait } from 'test';

export async function run() {
  assert(typeof wait === 'function', 'before await');
  const matcher = expect({ result: [2] });
  const same = {};
  expect(same).toBe(same);
  expect([]).toBeTruthy();
  expect({ result: [2] }).toEqual({ result: [2] });
  const compare = matcher.toEqual;
  const result = await wait('a');
  compare({ result: [result] });
  expect(result).toBe(2);
  return result;
}
