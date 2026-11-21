// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  let i = 1;
  expect(`${i++}${i++}
${null}`).toBe("12\nnull");
  expect(``).toBe("");
}
