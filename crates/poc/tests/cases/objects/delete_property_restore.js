// @pending ["wait:key"]
// @restore
// @fulfill ["wait:key", "first"]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  const sym = Symbol("first");
  const other = Symbol("second");
  let getterCalls = 0;
  function C() {}
  Object.defineProperty(C.prototype, "first", {value: 9});
  const object = new C();
  Object.defineProperty(object, "first", {
    configurable: true, enumerable: true, get() { getterCalls++; return 1; }
  });
  object.second = 2;
  object[sym] = 3;
  object[other] = 4;
  const array = [1, 2, 3];
  Object.defineProperty(array, 2, {configurable: false});
  expect(delete array[1]).toBe(true);
  expect(Reflect.deleteProperty(object, sym)).toBe(true);
  const remove = Reflect.deleteProperty;
  expect(delete (object?.[await wait("key")])).toBe(true);
  expect(getterCalls).toBe(0);
  expect(object.first).toBe(9);
  expect(object.hasOwnProperty("first")).toBe(false);
  expect(remove(object, "first")).toBe(true);
  expect("first" in object).toBe(true);
  expect(array.length).toBe(3);
  expect(1 in array).toBe(false);
  expect(remove(array, "length")).toBe(false);
  expect(remove(array, 2)).toBe(false);
  let error;
  try {
    delete array.length;
  } catch (caught) {
    error = caught;
  }
  expect(error?.name).toBe("TypeError");
  Object.defineProperty(object, "first", {value: 5, enumerable: true});
  object[sym] = 6;
  expect(Reflect.ownKeys(object)).toEqual(["second", "first", other, sym]);
  expect(array[2]).toBe(3);
}
