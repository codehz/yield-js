// @fulfilled undefined

import { expect, gc } from 'test';

function changingObject() {
  const object = {
    get first() {
      Object.defineProperty(this, "hiddenLater", { enumerable: false });
      Object.defineProperty(this, "shownLater", { enumerable: true });
      this.added = 4;
      return 1;
    },
    hiddenLater: 2,
  };
  Object.defineProperty(object, "shownLater", { value: 3, configurable: true });
  return object;
}

function checkProxy(method, expected) {
  const log = [];
  const symbol = Symbol("skip");
  const proxy = new Proxy({ a: 1, b: 2, [symbol]: 3 }, {
    ownKeys() {
      log.push("keys");
      return ["b", symbol, "a"];
    },
    getOwnPropertyDescriptor(target, key) {
      log.push("descriptor", key);
      return Reflect.getOwnPropertyDescriptor(target, key);
    },
    get(target, key, receiver) {
      expect(receiver).toBe(proxy);
      log.push("get", key);
      return Reflect.get(target, key, receiver);
    },
  });
  expect(method(proxy)).toEqual(expected);
  expect(log).toEqual(["keys", "descriptor", "b", "get", "b", "descriptor", "a", "get", "a"]);
}

export async function run() {
  expect(Object.values(changingObject())).toEqual([1, 3]);
  expect(Object.entries(changingObject())).toEqual([["first", 1], ["shownLater", 3]]);
  checkProxy(Object.values, [2, 1]);
  checkProxy(Object.entries, [["b", 2], ["a", 1]]);

  const sparse = [];
  sparse[2] = 7;
  expect(Object.values(sparse)).toEqual([7]);
  expect(Object.entries(sparse)).toEqual([["2", 7]]);
  expect(Object.values("😀")).toEqual(["\uD83D", "\uDE00"]);
  expect(Object.entries("😀")).toEqual([["0", "\uD83D"], ["1", "\uDE00"]]);

  for (const method of [Object.values, Object.entries]) {
    expect(method.length).toBe(1);
    expect(method(42)).toEqual([]);
    let error;
    try { method(null); } catch (caught) { error = caught; }
    expect(error?.name).toBe("TypeError");
  }

  const object = {
    get first() { return { kept: 5 }; },
    get second() { gc(); return 6; },
  };
  expect(Object.values(object)[0].kept).toBe(5);
  expect(Object.entries(object)[0][1].kept).toBe(5);
}
