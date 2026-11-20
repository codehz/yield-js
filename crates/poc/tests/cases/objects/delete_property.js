// @returns undefined

import { expect } from 'test';

export function run() {
  let log = "";
  const object = {x: 1};
  const key = {
    [Symbol.toPrimitive](hint) { log += hint; return "x"; }
  };
  function base() { log += "base/"; return object; }
  function index() { log += "key/"; return key; }
  const absent = null;
  expect(delete ((absent?.child[index()]))).toBe(true);
  expect(log).toBe("");
  expect(delete ((base()[index()]))).toBe(true);
  expect(log).toBe("base/key/string");
  expect("x" in object).toBe(false);
  expect(delete object.missing?.(index())).toBe(true);
  expect(log).toBe("base/key/string");
  let nullBaseError;
  try {
    delete (absent?.child)[index()];
  } catch (caught) {
    nullBaseError = caught;
  }
  expect(nullBaseError?.name).toBe("TypeError");
  expect(log).toBe("base/key/stringkey/");
  Object.defineProperty(object, "x", {value: 2});
  expect(Reflect.deleteProperty(object, key)).toBe(false);
  expect(log).toBe("base/key/stringkey/string");
  expect(object.x).toBe(2);
  let nonConfigurableError;
  try {
    delete object.x;
  } catch (caught) {
    nonConfigurableError = caught;
  }
  expect(nonConfigurableError?.name).toBe("TypeError");
  const bad = {toString() { throw 42; }};
  let coercionError;
  try {
    delete object[bad];
  } catch (caught) {
    coercionError = caught;
  }
  expect(coercionError).toBe(42);
  let reflectTargetError;
  try {
    Reflect.deleteProperty(1, bad);
  } catch (caught) {
    reflectTargetError = caught;
  }
  expect(reflectTargetError?.name).toBe("TypeError");
  let stringIndexError;
  try {
    delete "😀"[1];
  } catch (caught) {
    stringIndexError = caught;
  }
  expect(stringIndexError?.name).toBe("TypeError");
  expect(delete "😀"[2]).toBe(true);
  expect(delete "ab"["01"]).toBe(true);
  expect(delete true.x).toBe(true);
}
