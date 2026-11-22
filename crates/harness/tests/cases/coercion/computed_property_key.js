// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  let order = "";
  const symbol = Symbol("key");
  const key = {
    toString() { order += "key;"; return symbol; },
    valueOf() { order += "wrong;"; return "wrong"; }
  };
  function value() { order += "value;"; return 4; }

  const target = { [key]: value() };
  expect(order).toBe("key;value;");
  expect(target[symbol]).toBe(4);
  order = "";
  expect(target[key]).toBe(4);
  expect(key in target).toBe(true);
  expect(order).toBe("key;key;");

  order = "";
  const sum = (target[key] += value());
  expect(sum).toBe(8);
  expect(target[symbol]).toBe(8);
  expect(order).toBe("key;value;");

  order = "";
  Object.defineProperty(target, key, { value: 10 });
  expect(Object.hasOwn(target, key)).toBe(true);
  expect(Object.getOwnPropertyDescriptor(target, key).value).toBe(10);
  expect(target[symbol]).toBe(10);
  expect(order).toBe("key;key;key;");

  const fallback = {
    toString() { return {}; },
    valueOf() { return 9; }
  };
  target[fallback] = 3;
  expect(target["9"]).toBe(3);
  expect(fallback in target).toBe(true);

  order = "";
  function invalidRhs() { order += "rhs;"; return 1; }
  let invalidRhsError;
  try {
    key in invalidRhs();
  } catch (error) {
    invalidRhsError = error;
  }
  expect(invalidRhsError?.name).toBe("TypeError");
  expect(order).toBe("rhs;");

  order = "";
  const failure = {};
  const throwing = { toString() { throw failure; } };
  let keyError;
  try {
    ({ [throwing]: value() });
  } catch (error) {
    keyError = error;
  }
  expect(keyError).toBe(failure);
  expect(order).toBe("");
}
