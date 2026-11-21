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
  // TDZ is not implemented: entering the body block initializes `x` to
  // undefined before the statements run.
  expect(first).toBe(undefined);
  // The first closure keeps iteration 0's block binding, including its write.
  expect(second).toBe(9);
}
