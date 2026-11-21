// @fulfilled undefined

import { expect } from 'test';

function throwsTypeError(action) {
  try { action(); return false; } catch (e) { return e.name === "TypeError"; }
}

export async function run() {
  const a = Object.create(null, { value: { value: 1 } });
  const b = Object.create(a);
  const c = Object.create(b);
  expect(Reflect.setPrototypeOf(a, a)).toBe(false);
  expect(Reflect.setPrototypeOf(a, c)).toBe(false);
  expect(throwsTypeError(() => Object.setPrototypeOf(b, c))).toBe(true);
  expect(Object.getPrototypeOf(a)).toBe(null);
  expect(Object.getPrototypeOf(b)).toBe(a);
  expect(c.value).toBe(1);

  const replacement = { next: 2 };
  expect(Object.setPrototypeOf(b, replacement)).toBe(b);
  expect(Reflect.getPrototypeOf(b)).toBe(replacement);
  expect("value" in c).toBe(false);
  expect("next" in c).toBe(true);
  expect(c.next).toBe(2);
  expect(Reflect.setPrototypeOf(b, null)).toBe(true);
  expect(Object.getPrototypeOf(b)).toBe(null);
  expect("next" in c).toBe(false);

  const fixed = Object.freeze(c);
  expect(Reflect.setPrototypeOf(fixed, b)).toBe(true);
  expect(Object.setPrototypeOf(fixed, b)).toBe(fixed);
  expect(Reflect.setPrototypeOf(fixed, null)).toBe(false);
  expect(throwsTypeError(() => Object.setPrototypeOf(fixed, a))).toBe(true);
  expect(Object.getPrototypeOf(fixed)).toBe(b);

  const root = Object.prototype;
  expect(Object.isExtensible(root)).toBe(true);
  expect(Reflect.getPrototypeOf(root)).toBe(null);
  expect(Reflect.setPrototypeOf(root, null)).toBe(true);
  expect(Object.setPrototypeOf(root, null)).toBe(root);
  // A null-rooted candidate cannot introduce a cycle; the root link itself is immutable.
  expect(Reflect.setPrototypeOf(root, a)).toBe(false);
  expect(throwsTypeError(() => Object.setPrototypeOf(root, a))).toBe(true);
  expect(Reflect.getPrototypeOf(root)).toBe(null);
}
