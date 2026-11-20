// @returns undefined

import { expect } from 'test';

export function run() {
  const symbol = Symbol("key");
  const target = { [symbol]: 2, x: 1 };
  const receiver = { marker: 4 };
  let trace = "";
  let proxy;
  const handler = {
    get(t, k, r) {
      expect(this).toBe(handler);
      expect(t).toBe(target);
      expect(k).toBe(symbol);
      expect(r).toBe(receiver);
      trace += "g";
      return t[k] + r.marker;
    },
    set(t, k, v, r) {
      expect(this).toBe(handler);
      expect(t).toBe(target);
      expect(k).toBe("x");
      expect(v).toBe(9);
      expect(r).toBe(proxy);
      trace += "s";
      t[k] = v;
      return "accepted";
    },
    has(t, k) {
      expect(this).toBe(handler);
      expect(t).toBe(target);
      expect(k).toBe(symbol);
      trace += "h";
      return 1;
    },
    deleteProperty(t, k) {
      expect(this).toBe(handler);
      expect(t).toBe(target);
      expect(k).toBe("x");
      trace += "d";
      return delete t[k];
    },
    ownKeys(t, extra) {
      expect(this).toBe(handler);
      expect(t).toBe(target);
      expect(extra).toBe(undefined);
      trace += "k";
      return { length: 2, 0: symbol, 1: "x" };
    },
    getOwnPropertyDescriptor(t, k) {
      expect(this).toBe(handler);
      expect(t).toBe(target);
      expect(k).toBe(symbol);
      trace += "p";
      return Reflect.getOwnPropertyDescriptor(t, k);
    },
  };
  proxy = new Proxy(target, handler);
  const key = { [Symbol.toPrimitive]() { trace += "c"; return symbol; } };
  expect(Reflect.get(proxy, key, receiver)).toBe(6);
  proxy.x = 9;
  expect(target.x).toBe(9);
  expect(symbol in proxy).toBe(true);
  const keys = Reflect.ownKeys(proxy);
  expect(keys.length).toBe(2);
  expect(keys[0]).toBe(symbol);
  expect(keys[1]).toBe("x");
  const desc = Reflect.getOwnPropertyDescriptor(proxy, symbol);
  expect(desc.value).toBe(2);
  expect(desc.enumerable).toBe(true);
  expect(delete proxy.x).toBe(true);
  expect("x" in target).toBe(false);
  expect(trace).toBe("cgshkpd");
}
