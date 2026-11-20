// @pending ["wait:boxed"]
// @restore
// @fulfill ["wait:boxed", null]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  const box = Object(Symbol("only reachable through the wrapper"));
  const alias = { box };
  box.self = box;
  await wait("boxed");

  const symbol = box.valueOf();
  const keyed = { [symbol]: 42 };
  expect(alias.box).toBe(box);
  expect(box.self).toBe(box);
  expect(typeof symbol).toBe("symbol");
  expect(box.description).toBe("only reachable through the wrapper");
  expect(box.valueOf()).toBe(symbol);
  expect(Object(symbol).valueOf()).toBe(symbol);
  expect(Reflect.ownKeys(keyed)[0]).toBe(symbol);
  expect(keyed[box.valueOf()]).toBe(42);
  expect(Object.getPrototypeOf(box)).toBe(Symbol.prototype);
}
