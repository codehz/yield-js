// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  expect(+"foo" !== +"foo").toBe(true);
  expect(+"0x10").toBe(16);
  expect(+"0b1010").toBe(10);
  expect(+"0oZ" !== +"0oZ").toBe(true);
  expect(("foo" | 0)).toBe(0);
}
