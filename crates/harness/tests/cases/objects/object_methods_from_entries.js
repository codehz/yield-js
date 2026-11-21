// @fulfilled undefined

import { expect, gc } from 'test';

function expectThrow(action, check) {
  let thrown;
  try { action(); } catch (error) { thrown = error; }
  check(thrown);
}

function typeError(error) {
  expect(error?.name).toBe("TypeError");
}

function checkClose(next, count, check) {
  let closed = 0;
  const input = {
    [Symbol.iterator]() {
      return {
        next,
        return() {
          ++closed;
          gc();
          throw "close error";
        },
      };
    },
  };
  expectThrow(() => Object.fromEntries(input), check);
  expect(closed).toBe(count);
}

export async function run() {
  const log = [];
  const symbol = Symbol("entry");
  const entries = [{
    get 0() {
      log.push("read 0");
      return {
        [Symbol.toPrimitive](hint) {
          log.push(hint);
          gc();
          return "setterKey";
        },
      };
    },
    get 1() {
      log.push("read 1");
      gc();
      return { kept: 1 };
    },
  }, ["dup", 2], [symbol, 3], ["__proto__", 7], ["dup", 4]];
  const iterable = {
    [Symbol.iterator]() {
      let index = 0;
      return {
        next() {
          gc();
          return index < entries.length ? { value: entries[index++] } : { done: true };
        },
        return() { log.push("close"); return {}; },
      };
    },
  };
  Object.defineProperty(Object.prototype, "setterKey", {
    set(value) { log.push("setter"); },
    configurable: true,
  });
  let result;
  try {
    result = Object.fromEntries(iterable);
  } finally {
    delete Object.prototype.setterKey;
  }
  expect(log).toEqual(["read 0", "read 1", "string"]);
  expect(Reflect.ownKeys(result)).toEqual(["setterKey", "dup", "__proto__", symbol]);
  expect(result.setterKey.kept).toBe(1);
  expect(result.dup).toBe(4);
  expect(result[symbol]).toBe(3);
  expect(Object.getPrototypeOf(result)).toBe(Object.prototype);
  expect(Object.getOwnPropertyDescriptor(result, "__proto__")).toEqual({
    value: 7, writable: true, enumerable: true, configurable: true,
  });

  checkClose(() => ({ value: 1 }), 1, typeError);
  checkClose(() => ({ value: [{ toString() { throw { message: "bad key" }; } }, 1] }),
    1, error => expect(error?.message).toBe("bad key"));
  checkClose(() => ({ value: { get 1() { throw "bad value"; } } }),
    1, error => expect(error).toBe("bad value"));
  checkClose(() => { throw "next error"; }, 0, error => expect(error).toBe("next error"));

  expect(Object.fromEntries.length).toBe(1);
  expectThrow(() => Object.fromEntries(), typeError);
  expectThrow(() => Object.fromEntries(null), typeError);
  expectThrow(() => Object.fromEntries({}), typeError);
  const empty = Object.fromEntries([]);
  expect(Reflect.ownKeys(empty)).toEqual([]);
  expect(Object.getPrototypeOf(empty)).toBe(Object.prototype);
}
