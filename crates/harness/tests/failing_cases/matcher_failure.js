// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a",3]
// @fulfilled 3

import { expect, wait } from 'test';

export async function run() {
  const actual = await wait('a');
  try { expect(actual).toBe(4); } catch (error) { throw error.message; }
  return actual;
}
