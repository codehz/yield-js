// @returns undefined

import { expect } from 'test';

export function run() {
  expect(typeof JSON).toBe("object");
  expect(JSON.prototype).toBe(undefined);
  let callError;
  try {
    JSON();
  } catch (error) {
    callError = error;
  }
  expect(callError instanceof TypeError).toBe(true);

  let constructError;
  try {
    new JSON();
  } catch (error) {
    constructError = error;
  }
  expect(constructError instanceof TypeError).toBe(true);
  expect(JSON.stringify(null)).toBe("null");
  expect(JSON.stringify(true)).toBe("true");
  expect(JSON.stringify(1.5)).toBe("1.5");
  expect(JSON.stringify(-0)).toBe("0");
  expect(JSON.stringify("a\"b\\")).toBe("\"a\\\"b\\\\\"");
  expect(JSON.stringify("\u0001")).toBe("\"\\u0001\"");
  expect(JSON.stringify(undefined)).toBe(undefined);
  expect(JSON.stringify(() => 1)).toBe(undefined);
  expect(JSON.stringify(Symbol())).toBe(undefined);
  expect(JSON.stringify(NaN)).toBe("null");
  expect(JSON.stringify(Infinity)).toBe("null");
  const parsed = JSON.parse('{"b":1,"a":[true,null,false],"__proto__":{"x":1}}');
  expect(parsed.b).toBe(1);
  expect(parsed.a[2]).toBe(false);
  expect(Object.getPrototypeOf(parsed)).toBe(Object.prototype);
  expect(parsed.__proto__.x).toBe(1);
  expect(Object.getPrototypeOf(parsed) === parsed.__proto__).toBe(false);
  let incompleteError;
  try {
    JSON.parse("{");
  } catch (error) {
    incompleteError = error;
  }
  expect(incompleteError instanceof SyntaxError).toBe(true);

  let trailingInputError;
  try {
    JSON.parse("1 2");
  } catch (error) {
    trailingInputError = error;
  }
  expect(trailingInputError instanceof SyntaxError).toBe(true);
}
