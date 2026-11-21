// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  let x = 0;
  let skipped = (x &&= await wait("skip"));
  expect(skipped).toBe(0);
  expect(x).toBe(0);
  let still = (x ??= 9);
  expect(still).toBe(0);
  expect(x).toBe(0);
  let assigned = (x ||= 4);
  expect(assigned).toBe(4);
  expect(x).toBe(4);
  let kept = (x ||= await wait("skip"));
  expect(kept).toBe(4);
  expect(x).toBe(4);
  let updated = (x &&= 5);
  expect(updated).toBe(5);
  expect(x).toBe(5);

  x = null;
  let fromNull = (x ??= 6);
  expect(fromNull).toBe(6);
  expect(x).toBe(6);
  let missing;
  x = missing;
  let fromMissing = (x ??= 6);
  expect(fromMissing).toBe(6);
  expect(x).toBe(6);
}
