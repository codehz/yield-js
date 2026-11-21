// @pending ["wait:keys"]
// @restore
// @fulfill ["wait:keys", 0]
// @fulfilled undefined

import { expect, wait } from 'test';

function Box() {}

export async function run() {
  Box.prototype.inherited = 1;
  const object = new Box();
  const symbol = Symbol("extra");
  object.z = 1;
  object[symbol] = 2;
  object[10] = 3;
  object.a = 4;
  object[2] = 5;
  object.z = 6;
  const accessor = { get read() { throw "enumeration read the getter"; } };
  const array = [1, 2];
  array.extra = 3;
  array[symbol] = 4;
  const keys = Object.keys;
  Reflect.ownKeys = Reflect.ownKeys;
  await wait("keys");
  expect(keys).toBe(Object.keys);
  expect(keys(object)).toEqual(["2", "10", "z", "a"]);
  expect(keys(accessor)[0]).toBe("read");
  expect(keys(array)).toEqual(["0", "1", "extra"]);
  expect(keys(Reflect).length).toBe(0);
  expect(Reflect.ownKeys(Reflect)[0]).toBe("ownKeys");
  expect(keys(Box).length).toBe(0);
  expect(keys(Object).length).toBe(0);
  expect(keys(Object.prototype).length).toBe(0);
  let error;
  try {
    keys(null);
  } catch (caught) {
    error = caught;
  }
  expect(error?.name).toBe("TypeError");
  const stringKeys = keys("a😀");
  expect(stringKeys.length).toBe(3);
  expect(stringKeys[0]).toBe("0");
  expect(stringKeys[2]).toBe("2");
  expect(keys(1).length).toBe(0);
  expect(keys(symbol).length).toBe(0);
}
