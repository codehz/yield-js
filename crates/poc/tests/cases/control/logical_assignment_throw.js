// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  let o = { v: 1 };
  let thrown;
  try {
    o.v &&= fail();
  } catch (error) {
    thrown = error;
  }
  expect(thrown).toBe("boom");
  expect(o.v).toBe(1);
}

function fail() {
  throw "boom";
}
