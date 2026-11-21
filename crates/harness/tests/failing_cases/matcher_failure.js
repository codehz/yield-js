// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a",3]
// @fulfilled 3

import { expect, wait } from 'test';

export async function run() {
  const actual = await wait('a');
  expect(actual).toBe(4);
  return actual;
}
