// @returns undefined

import { expect } from 'test';

export function run() {
  const symbol = Symbol("copy");
  const target = { a: 1, b: 2, hidden: 3, [symbol]: 4 };
  const source = new Proxy(target, {
    ownKeys() { return [symbol, "b", "hidden", "a", "missing"]; },
    getOwnPropertyDescriptor(t, k) {
      if (k === "hidden") return { value: 3, configurable: true };
      return Reflect.getOwnPropertyDescriptor(t, k);
    },
    get(t, k, r) { return k === "a" ? 10 : Reflect.get(t, k, r); },
  });
  expect(Object.keys(source).join("|")).toBe("b|a");
  expect(source.hasOwnProperty("hidden")).toBe(true);
  expect(source.hasOwnProperty("missing")).toBe(false);
  expect(JSON.stringify(source)).toBe('{"b":2,"a":10}');
  const descriptors = Object.getOwnPropertyDescriptors(source);
  expect(descriptors.a.value).toBe(1);
  expect(descriptors.hidden.enumerable).toBe(false);
  expect(descriptors[symbol].value).toBe(4);
  expect("missing" in descriptors).toBe(false);
  const spread = { ...source };
  const { a, ...rest } = source;
  expect(a).toBe(10);
  expect(spread.a).toBe(10);
  expect(spread[symbol]).toBe(4);
  expect("hidden" in spread).toBe(false);
  expect("a" in rest).toBe(false);
  expect(rest.b).toBe(2);
  expect(rest[symbol]).toBe(4);
  const writes = [];
  const assigned = new Proxy({}, {
    set(t, k, v) { writes.push(k); t[k] = v; return true; },
  });
  expect(Object.assign(assigned, source)).toBe(assigned);
  expect(writes.length).toBe(3);
  expect(writes[0]).toBe(symbol);
  expect(writes[1]).toBe("b");
  expect(writes[2]).toBe("a");
  expect(assigned.a).toBe(10);
}
