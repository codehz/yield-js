// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  let read;
  let first;
  let second;
  for (let i = 0; i < 2; i = i + 1) {
    if (i === 0) {
      read = () => x;
      first = read();
    } else {
      second = read();
    }
    let x;
    x = 9;
  }
  expect(first).toBe(undefined);
  expect(second).toBe(undefined);
}
