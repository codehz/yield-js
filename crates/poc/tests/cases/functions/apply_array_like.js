// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  // Plain array-like object.
  expect(first.apply(null, { 0: "a", length: 1 })).toBe("a");

  // A real Array still works.
  expect(first.apply(null, ["x", "y"])).toBe("x");

  // null and undefined mean an empty argument list.
  expect(first.apply(null)).toBe(undefined);
  expect(first.apply(null, null)).toBe(undefined);
  expect(first.apply(null, undefined)).toBe(undefined);

  // An arguments object works.
  function spreadArgs() {
    return count.apply(null, arguments);
  }
  expect(spreadArgs("p", "q", "r")).toBe(3);

  // Inherited index and length properties are used.
  const proto = { 0: "inherited", length: 2 };
  const child = Object.create(proto);
  child[1] = "own";
  expect(pair.apply(null, child)).toBe("inherited-own");

  // A getter access from the array-like propagates its throw.
  const throwingLength = {
    get length() {
      throw new RangeError("length boom");
    },
  };
  let lengthError;
  try {
    first.apply(null, throwingLength);
  } catch (e) {
    lengthError = e;
  }
  expect(lengthError?.name).toBe("RangeError");
  expect(lengthError?.message).toBe("length boom");
  const throwingIndex = { length: 2, 0: "ok" };
  Object.defineProperty(throwingIndex, "1", {
    get() {
      throw new TypeError("index boom");
    },
  });
  let indexError;
  try {
    first.apply(null, throwingIndex);
  } catch (e) {
    indexError = e;
  }
  expect(indexError?.name).toBe("TypeError");
  expect(indexError?.message).toBe("index boom");

  // A primitive argument list throws a catchable TypeError.
  let stringError;
  try {
    first.apply(null, "ab");
  } catch (e) {
    stringError = e;
  }
  expect(stringError?.name).toBe("TypeError");
  let numberError;
  try {
    first.apply(null, 5);
  } catch (e) {
    numberError = e;
  }
  expect(numberError?.name).toBe("TypeError");

  // ToLength coercion of the length value.
  const weird = { length: "2.9", 0: "u", 1: "v" };
  expect(pair.apply(null, weird)).toBe("u-v");
}

function first(a) {
  return a;
}

function pair(a, b) {
  return a + "-" + b;
}

function count(...args) {
  return args.length;
}
