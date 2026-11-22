// @fulfilled undefined

import { expect, gc } from 'test';

function typeError(error) {
  expect(error?.name).toBe("TypeError");
}

export async function run() {
  const symbol = Symbol("odd");
  const calls = [];
  const result = Object.groupBy([1, 2, 3, 4, 5], function (value, index) {
    calls.push([value, index, this]);
    gc();
    return value % 2 === 1 ? symbol : "even";
  });
  expect(calls.map(call => call[0])).toEqual([1, 2, 3, 4, 5]);
  expect(calls.map(call => call[1])).toEqual([0, 1, 2, 3, 4]);
  expect(calls.every(call => call[2] === undefined)).toBe(true);
  expect(Object.getPrototypeOf(result)).toBe(null);
  expect(Object.keys(result)).toEqual(["even"]);
  expect(result.even).toEqual([2, 4]);
  expect(result[symbol]).toEqual([1, 3, 5]);
  expect(Object.getOwnPropertyDescriptor(result, "even")).toEqual({
    value: result.even,
    writable: true,
    enumerable: true,
    configurable: true,
  });

  const keyed = Object.groupBy([1, 2, 3], value => ({
    toString() {
      gc();
      return "k" + (value % 2);
    },
  }));
  expect(Reflect.ownKeys(keyed)).toEqual(["k1", "k0"]);
  expect(keyed.k1).toEqual([1, 3]);
  expect(keyed.k0).toEqual([2]);

  const proto = Object.groupBy(["a", "b"], value => "__proto__");
  expect(Object.getPrototypeOf(proto)).toBe(null);
  expect(Reflect.ownKeys(proto)).toEqual(["__proto__"]);
  expect(proto.__proto__).toEqual(["a", "b"]);

  const empty = Object.groupBy([], () => "unused");
  expect(Object.getPrototypeOf(empty)).toBe(null);
  expect(Reflect.ownKeys(empty)).toEqual([]);

  for (const action of [
    () => Object.groupBy(null, () => "x"),
    () => Object.groupBy(undefined, () => "x"),
    () => Object.groupBy([], undefined),
    () => Object.groupBy([], 1),
    () => Object.groupBy({}, () => "x"),
  ]) {
    let error;
    try {
      action();
    } catch (caught) {
      error = caught;
    }
    typeError(error);
  }

  let closed = 0;
  const marker = { marker: true };
  const throwingCallback = {
    [Symbol.iterator]() {
      return {
        next() {
          return { value: 1, done: false };
        },
        return() {
          closed += 1;
          gc();
          throw "close error";
        },
      };
    },
  };
  let callbackError;
  try {
    Object.groupBy(throwingCallback, () => {
      throw marker;
    });
  } catch (caught) {
    callbackError = caught;
  }
  expect(callbackError).toBe(marker);
  expect(closed).toBe(1);

  closed = 0;
  let keyError;
  try {
    Object.groupBy(throwingCallback, () => ({
      toString() {
        gc();
        throw marker;
      },
    }));
  } catch (caught) {
    keyError = caught;
  }
  expect(keyError).toBe(marker);
  expect(closed).toBe(1);

  let nextClosed = 0;
  const brokenNext = {
    [Symbol.iterator]() {
      return {
        next() {
          throw "next error";
        },
        return() {
          nextClosed += 1;
          return {};
        },
      };
    },
  };
  let stepError;
  try {
    Object.groupBy(brokenNext, () => "x");
  } catch (caught) {
    stepError = caught;
  }
  expect(stepError).toBe("next error");
  expect(nextClosed).toBe(0);

  expect(Object.groupBy.length).toBe(2);
  expect(typeof Object.groupBy).toBe("function");
}
