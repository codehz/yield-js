// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  let n = 0;
  const obj = { x: 7 };
  const result = delete (n++, obj.x);
  expect(result).toBe(true);
  expect(n).toBe(1);
  expect(obj.x).toBe(7);
}
