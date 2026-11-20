// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  let n = 0;
  let v = null?.[++n].m(++n);
  let u = undefined?.(++n);
  expect(v).toBe(undefined);
  expect(u).toBe(undefined);
  expect(n).toBe(0);
}
