// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  const value = { valueOf() { return 7; } };
  let caught;
  try {
    ({ valueOf() { throw new TypeError("conversion failed"); } }) - 1;
  } catch (error) {
    caught = error;
  }
  expect(value + 1).toBe(8);
  expect(+{} !== +{}).toBe(true);
  expect(caught?.name).toBe("TypeError");
}
