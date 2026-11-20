// @restore
// @reject ["wait:a",5]
// @fulfilled 15

import { expect, wait } from 'test';

export async function run() {
  let base = 10;
  expect(base).toBeTruthy();
  try {
    await wait("a");
    return 0;
  } catch (e) {
    expect(e).toBe(5);
    return base + e;
  }
}
