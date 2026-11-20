// @pending ["wait:array descriptor"]
// @restore
// @fulfill ["wait:array descriptor", 0]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  const define = Object.defineProperty;
  const describe = Object.getOwnPropertyDescriptor;
  const array = [];
  define(array, "4294967294", { value: 7, configurable: true });
  expect(array.length).toBe(4294967295);
  expect(0 in array).toBe(false);
  expect(Object.keys(array).length).toBe(0);
  define(array, "length", { value: "4" });
  define(array, 1, { get: function () { return this.length; }, enumerable: true });
  array[3] = 9;
  await wait("array descriptor");
  expect(Object.keys(array).length).toBe(2);
  expect(Reflect.ownKeys(array)[2]).toBe("length");
  const copy = [...array];
  expect(copy.length).toBe(4);
  expect(copy[0]).toBe(undefined);
  expect(copy[1]).toBe(4);
  expect(copy[3]).toBe(9);
  let shrinkError;
  try {
    define(array, "length", { value: 0, writable: false });
  } catch (error) {
    shrinkError = error;
  }
  expect(shrinkError?.name).toBe("TypeError");
  expect(array.length).toBe(2);
  expect(3 in array).toBe(false);
  expect(describe(array, "length").writable).toBe(false);
  let assignmentError;
  try {
    array[2] = 1;
  } catch (error) {
    assignmentError = error;
  }
  expect(assignmentError?.name).toBe("TypeError");
  let extensionError;
  try {
    define(array, 2, { value: 1 });
  } catch (error) {
    extensionError = error;
  }
  expect(extensionError?.name).toBe("TypeError");
  let lengthError;
  try {
    define(array, "length", { value: 0.5 });
  } catch (error) {
    lengthError = error;
  }
  expect(lengthError?.name).toBe("RangeError");
  define(array, 0, { value: 6 });
  expect(array.length).toBe(2);
  expect(array[0]).toBe(6);
  expect(array[1]).toBe(2);
}
