// @pending ["wait:key coercion"]
// @restore
// @fulfill ["wait:key coercion", 5]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  const symbol = Symbol("saved key");
  const failure = {};
  let conversions = 0;
  const key = {
    toString() {
      conversions++;
      if (conversions > 1) throw failure;
      return symbol;
    }
  };
  const target = { [symbol]: 2 };
  const sum = (target[key] += await wait(conversions === 1 ? "key coercion" : "unconverted key"));
  expect(sum).toBe(7);
  expect(target[symbol]).toBe(7);
  expect(conversions).toBe(1);
  let thrown;
  try {
    target[key];
  } catch (error) {
    thrown = error;
  }
  expect(thrown).toBe(failure);
  expect(conversions).toBe(2);
  expect(sum).toBe(7);
  expect(target[symbol]).toBe(7);
}
