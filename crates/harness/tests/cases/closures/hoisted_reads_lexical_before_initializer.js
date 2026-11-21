// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  function read() {
    return x;
  }
  let before = read();
  let x = 7;
  let after = read();
  expect(before).toBe(undefined);
  expect(after).toBe(7);
}
