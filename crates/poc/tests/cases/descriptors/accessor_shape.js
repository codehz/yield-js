// @pending ["wait:descriptor"]
// @restore
// @fulfill ["wait:descriptor", 0]
// @fulfilled undefined

import { expect, wait } from 'test';

function Box() {}

export async function run() {
  const symbol = Symbol("data");
  const object = {
    [symbol]: 1,
    get x() { throw "descriptor lookup read the getter"; },
    set x(value) { this[symbol] = value; },
  };
  const describe = Object.getOwnPropertyDescriptor;
  const before = describe(object, "x");
  const array = [1];
  await wait("descriptor");
  const accessor = describe(object, "x");
  expect(accessor.get).toBe(before.get);
  expect(accessor.set).toBe(before.set);
  expect(typeof accessor.get).toBe("function");
  expect(accessor.enumerable).toBe(true);
  expect(accessor.configurable).toBe(true);
  expect("writable" in accessor).toBe(false);
  expect("value" in accessor).toBe(false);
  object.x = 7;
  const data = describe(object, symbol);
  expect(data.value).toBe(7);
  expect(data.writable).toBe(true);
  expect(data.enumerable).toBe(true);
  expect(data.configurable).toBe(true);
  expect("get" in data).toBe(false);
  expect(describe(object, "hasOwnProperty")).toBe(undefined);
  data.value = 100;
  expect(object[symbol]).toBe(7);
  const length = describe(array, "length");
  const element = describe(array, "0");
  expect(length.value).toBe(1);
  expect(length.writable).toBe(true);
  expect(length.enumerable).toBe(false);
  expect(length.configurable).toBe(false);
  expect(element.enumerable).toBe(true);
  expect(element.configurable).toBe(true);
  expect(element.writable).toBe(true);
  const method = describe(Object, "keys");
  expect(method.value).toBe(Object.keys);
  expect(method.writable).toBe(true);
  expect(method.enumerable).toBe(false);
  expect(method.configurable).toBe(true);
  const fixed = describe(Object, "prototype");
  expect(fixed.writable).toBe(false);
  expect(fixed.enumerable).toBe(false);
  expect(fixed.configurable).toBe(false);
  expect(fixed.value).toBe(Object.prototype);
  let prototypeError;
  try {
    Object.prototype = {};
  } catch (error) {
    prototypeError = error;
  }
  expect(prototypeError?.name).toBe("TypeError");
  Box.prototype = Symbol;
  const box = new Box();
  let iteratorError;
  try {
    box.iterator = 1;
  } catch (error) {
    iteratorError = error;
  }
  expect(iteratorError?.name).toBe("TypeError");
  expect(box.iterator).toBe(Symbol.iterator);
  expect(describe(box, "iterator")).toBe(undefined);
}
