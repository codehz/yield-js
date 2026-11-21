// @returns undefined

import { expect } from 'test';

function typeError(fn) {
  try { fn(); } catch (e) { return e.name === "TypeError"; }
  return false;
}

export function run() {
  expect(typeError(() => Proxy({}, {}))).toBe(true);
  expect(typeError(() => new Proxy())).toBe(true);
  expect(typeError(() => new Proxy(1, {}))).toBe(true);
  expect(typeError(() => new Proxy({}, null))).toBe(true);
  expect(typeError(() => new Proxy(() => 1, {}))).toBe(true);
  expect(typeError(() => new Proxy(new Map(), {}))).toBe(true);

  const array = [1, , 3];
  const handler = { get: null, set: undefined, deleteProperty: null, ownKeys: undefined };
  const proxy = Reflect.construct(Proxy, [array, handler]);
  expect(typeError(() => proxy())).toBe(true);
  expect(typeError(() => new proxy())).toBe(true);
  expect(proxy.length).toBe(3);
  expect(proxy[0]).toBe(1);
  expect(1 in proxy).toBe(false);
  expect(2 in proxy).toBe(true);
  expect(Reflect.ownKeys(proxy).join("|")).toBe("0|2|length");
  expect(Reflect.getOwnPropertyDescriptor(proxy, "length").enumerable).toBe(false);
  proxy[4] = 5;
  expect(array.length).toBe(5);
  expect(array[4]).toBe(5);
  expect(Reflect.deleteProperty(proxy, 2)).toBe(true);
  expect(2 in array).toBe(false);
  expect(array.length).toBe(5);
  expect(Reflect.set(proxy, "length", 1)).toBe(true);
  expect(array.length).toBe(1);
  expect(4 in array).toBe(false);

  const target = { get x() { return this.marker; }, set x(v) { this.marker = v; }, marker: 2 };
  let lookups = 0;
  const inherited = { get get() { lookups += 1; return null; } };
  const dynamic = Object.create(inherited);
  const p = new Proxy(target, dynamic);
  const receiver = { marker: 3 };
  // The getter's this.marker lookup also goes through the proxy.
  expect(Reflect.get(p, "x", receiver)).toBe(3);
  expect(p.x).toBe(2);
  expect(lookups).toBe(3);
  expect(Reflect.set(p, "x", 4, receiver)).toBe(true);
  expect(receiver.marker).toBe(4);
  Object.defineProperty(dynamic, "get", { value: 1, configurable: true });
  expect(typeError(() => p.x)).toBe(true);
  Object.defineProperty(dynamic, "get", { get() { throw "lookup"; }, configurable: true });
  let lookupError;
  try { p.x; } catch (e) { lookupError = e; }
  expect(lookupError).toBe("lookup");
  Object.defineProperty(dynamic, "get", { value() { throw "trap"; }, configurable: true });
  let trapError;
  try { p.x; } catch (e) { trapError = e; }
  expect(trapError).toBe("trap");
}
