// @returns undefined

import { expect } from 'test';

function typeError(fn) {
  try { fn(); } catch (e) { return e.name === "TypeError"; }
  return false;
}

export function run() {
  const fixed = Object.defineProperties({}, {
    zero: { value: -0 },
    nan: { value: NaN },
    accessor: { get: undefined, set: undefined },
  });
  const handler = {
    get(t, k) { return k === "zero" ? 0 : k === "nan" ? NaN : 1; },
    set() { return true; },
    has() { return false; },
    deleteProperty() { return true; },
    ownKeys() { return ["zero"]; },
    getOwnPropertyDescriptor() { return undefined; },
  };
  const p = new Proxy(fixed, handler);
  expect(typeError(() => p.zero)).toBe(true);
  expect(Object.is(p.nan, NaN)).toBe(true);
  expect(typeError(() => p.accessor)).toBe(true);
  expect(typeError(() => Reflect.set(p, "zero", 0))).toBe(true);
  expect(Reflect.set(p, "zero", -0)).toBe(true);
  expect(typeError(() => Reflect.set(p, "accessor", undefined))).toBe(true);
  expect(typeError(() => "zero" in p)).toBe(true);
  expect(typeError(() => Reflect.deleteProperty(p, "zero"))).toBe(true);
  expect(typeError(() => Reflect.ownKeys(p))).toBe(true);
  expect(typeError(() => Reflect.getOwnPropertyDescriptor(p, "zero"))).toBe(true);

  const t = { present: 1 };
  const h = {
    set() { return 0; },
    deleteProperty() { return ""; },
    has() { return false; },
    ownKeys() { return ["present", "present"]; },
    getOwnPropertyDescriptor() { return 0; },
  };
  const q = new Proxy(t, h);
  expect(Reflect.set(q, "present", 2)).toBe(false);
  expect(Reflect.deleteProperty(q, "present")).toBe(false);
  expect(typeError(() => { q.present = 2; })).toBe(true);
  expect(typeError(() => delete q.present)).toBe(true);
  expect(t.present).toBe(1);
  expect(typeError(() => Reflect.ownKeys(q))).toBe(true);
  expect(typeError(() => Object.getOwnPropertyDescriptor(q, "present"))).toBe(true);
  h.ownKeys = () => [1];
  expect(typeError(() => Reflect.ownKeys(q))).toBe(true);
  h.ownKeys = () => null;
  expect(typeError(() => Reflect.ownKeys(q))).toBe(true);
  h.getOwnPropertyDescriptor = () => ({ value: 1 });
  expect(typeError(() => Object.getOwnPropertyDescriptor(q, "present"))).toBe(true);

  Object.preventExtensions(t);
  h.deleteProperty = () => true;
  h.ownKeys = () => ["present", "extra"];
  h.getOwnPropertyDescriptor = () => ({ configurable: true });
  expect(typeError(() => Reflect.has(q, "present"))).toBe(true);
  expect(typeError(() => delete q.present)).toBe(true);
  expect(typeError(() => Reflect.ownKeys(q))).toBe(true);
  expect(typeError(() => Reflect.getOwnPropertyDescriptor(q, "extra"))).toBe(true);
  h.ownKeys = () => [];
  expect(typeError(() => Reflect.ownKeys(q))).toBe(true);
  h.ownKeys = () => ["present"];
  h.getOwnPropertyDescriptor = () => undefined;
  expect(Reflect.ownKeys(q)[0]).toBe("present");
  expect(typeError(() => Reflect.getOwnPropertyDescriptor(q, "present"))).toBe(true);

  const array = Object.freeze([1]);
  const a = new Proxy(array, {
    ownKeys() { return ["0"]; },
    get() { return 2; },
    getOwnPropertyDescriptor() { return { value: 1, writable: true }; },
  });
  expect(typeError(() => Reflect.ownKeys(a))).toBe(true);
  expect(typeError(() => a.length)).toBe(true);
  expect(typeError(() => Object.getOwnPropertyDescriptor(a, "0"))).toBe(true);
}
