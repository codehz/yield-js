// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  const symbol = Symbol("own");
  const inherited = Object.create({ a: 1 });
  inherited.b = 2;
  inherited[symbol] = 3;
  Object.defineProperty(inherited, "hidden", { value: 4 });

  expect(Object.hasOwn(inherited, "a")).toBe(false);
  expect(Object.hasOwn(inherited, "b")).toBe(true);
  expect(Object.hasOwn(inherited, "hidden")).toBe(true);
  expect(Object.hasOwn(inherited, symbol)).toBe(true);
  expect(Object.hasOwn(inherited, "toString")).toBe(false);

  expect(Object.hasOwn("a😀", "0")).toBe(true);
  expect(Object.hasOwn("a😀", "2")).toBe(true);
  expect(Object.hasOwn("a😀", "3")).toBe(false);
  expect(Object.hasOwn("a😀", "length")).toBe(true);
  expect(Object.hasOwn(1, "toString")).toBe(false);

  let conversions = 0;
  const key = {
    toString() {
      conversions += 1;
      return "b";
    },
  };
  expect(Object.hasOwn(inherited, key)).toBe(true);
  expect(conversions).toBe(1);

  let unreached = 0;
  const throwingKey = {
    toString() {
      unreached += 1;
      throw "unreachable";
    },
  };
  let nullishError;
  try {
    Object.hasOwn(null, throwingKey);
  } catch (caught) {
    nullishError = caught;
  }
  expect(nullishError?.name).toBe("TypeError");
  expect(unreached).toBe(0);

  let keyError;
  try {
    Object.hasOwn({}, throwingKey);
  } catch (caught) {
    keyError = caught;
  }
  expect(keyError).toBe("unreachable");

  let trapped = 0;
  const proxy = new Proxy(inherited, {
    getOwnPropertyDescriptor(target, property) {
      trapped += 1;
      return Reflect.getOwnPropertyDescriptor(target, property);
    },
  });
  expect(Object.hasOwn(proxy, "b")).toBe(true);
  expect(Object.hasOwn(proxy, "a")).toBe(false);
  expect(trapped).toBe(2);

  expect(Object.hasOwn.length).toBe(2);
  expect(typeof Object.hasOwn).toBe("function");
}
