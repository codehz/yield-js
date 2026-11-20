// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  let x = "4";
  let a = x++;
  let b = --x;
  let c = ++x;
  let d = x--;
  expect(a).toBe(4);
  expect(b).toBe(4);
  expect(c).toBe(5);
  expect(d).toBe(5);
  expect(x).toBe(4);
}
