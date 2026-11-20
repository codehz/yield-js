// @fulfilled undefined

import { expect, bind } from 'test';

function throwsTypeError(action) {
  try { action(); return false; } catch (e) { return e.name === "TypeError"; }
}

export async function run() {
  const sym = Symbol("primitive");
  for (const value of [1, "text", false, sym]) {
    expect(Object.setPrototypeOf(value, null)).toBe(value);
    expect(Object.getPrototypeOf(value)).toBe(Object.getPrototypeOf(Object(value)));
    expect(throwsTypeError(() => Reflect.getPrototypeOf(value))).toBe(true);
    expect(throwsTypeError(() => Reflect.setPrototypeOf(value, null))).toBe(true);
    expect(throwsTypeError(() => Object.create(value))).toBe(true);
  }
  expect(throwsTypeError(() => Object.setPrototypeOf(1, 2))).toBe(true);
  for (const value of [null, undefined]) {
    expect(throwsTypeError(() => Object.setPrototypeOf(value, null))).toBe(true);
    expect(throwsTypeError(() => Object.getPrototypeOf(value))).toBe(true);
    expect(throwsTypeError(() => Reflect.getPrototypeOf(value))).toBe(true);
    expect(throwsTypeError(() => Reflect.setPrototypeOf(value, null))).toBe(true);
  }
  const target = {};
  expect(throwsTypeError(() => Reflect.setPrototypeOf(target, undefined))).toBe(true);
  expect(throwsTypeError(() => Object.setPrototypeOf(target, false))).toBe(true);
  expect(Object.getPrototypeOf(target)).toBe(Object.prototype);

  const native = bind("prototype-boundary");
  expect(Object.getPrototypeOf(native) === null).toBe(false);
  expect(Reflect.setPrototypeOf(native, null)).toBe(true);
  expect(Object.setPrototypeOf(native, target)).toBe(native);
  const child = Object.create(native);
  expect(Object.getPrototypeOf(child)).toBe(native);
  expect("toString" in child).toBe(true);
}
