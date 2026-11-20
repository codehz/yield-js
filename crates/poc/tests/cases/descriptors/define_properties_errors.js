// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  const define = Object.defineProperties;
  const target = {};
  const error = {};
  let conversionError;
  try {
    define(target, { a: { value: 1 }, get b() { target.sideEffect = 2; throw error; } });
  } catch (caught) {
    conversionError = caught;
  }
  expect(conversionError).toBe(error);
  expect("a" in target).toBe(false);
  expect(target.sideEffect).toBe(2);
  Object.defineProperty(target, "fixed", { value: 1 });
  let converted = false;
  let fixedError;
  try {
    define(target, {
      a: { value: 3 },
      fixed: { value: 2 },
      c: { get value() { converted = true; return 4; } },
    });
  } catch (caught) {
    fixedError = caught;
  }
  expect(fixedError?.name).toBe("TypeError");
  expect(converted).toBe(true);
  expect(target.a).toBe(3);
  expect(target.fixed).toBe(1);
  expect("c" in target).toBe(false);
  let invalidError;
  try {
    define(target, { b: { value: 5 }, invalid: 0 });
  } catch (caught) {
    invalidError = caught;
  }
  expect(invalidError?.name).toBe("TypeError");
  expect("b" in target).toBe(false);
  let stringError;
  try {
    define(target, "x");
  } catch (caught) {
    stringError = caught;
  }
  expect(stringError?.name).toBe("TypeError");
  expect("0" in target).toBe(false);
  let nullError;
  try {
    define(target, null);
  } catch (caught) {
    nullError = caught;
  }
  expect(nullError?.name).toBe("TypeError");
  let targetError;
  try {
    define(1, { get a() { throw error; } });
  } catch (caught) {
    targetError = caught;
  }
  expect(targetError?.name).toBe("TypeError");
  expect(define(target, 1)).toBe(target);
  expect(define(target, "")).toBe(target);
}
