// @fulfilled true

import { bind } from 'test';

function throwsTypeError(action) {
  try { action(); return false; } catch (e) { return e.name === "TypeError"; }
}

export async function run() {
  const sym = Symbol("primitive");
  for (const value of [1, "text", false, sym]) {
    if (Object.setPrototypeOf(value, null) !== value) return false;
    if (Object.getPrototypeOf(value) !== Object.getPrototypeOf(Object(value))) return false;
    if (!throwsTypeError(() => Reflect.getPrototypeOf(value))) return false;
    if (!throwsTypeError(() => Reflect.setPrototypeOf(value, null))) return false;
    if (!throwsTypeError(() => Object.create(value))) return false;
  }
  if (!throwsTypeError(() => Object.setPrototypeOf(1, 2))) return false;
  for (const value of [null, undefined]) {
    if (!throwsTypeError(() => Object.setPrototypeOf(value, null))) return false;
    if (!throwsTypeError(() => Object.getPrototypeOf(value))) return false;
    if (!throwsTypeError(() => Reflect.getPrototypeOf(value))) return false;
    if (!throwsTypeError(() => Reflect.setPrototypeOf(value, null))) return false;
  }
  const target = {};
  if (!throwsTypeError(() => Reflect.setPrototypeOf(target, undefined))) return false;
  if (!throwsTypeError(() => Object.setPrototypeOf(target, false))) return false;
  if (Object.getPrototypeOf(target) !== Object.prototype) return false;

  const native = bind("prototype-boundary");
  if (Object.getPrototypeOf(native) === null) return false;
  if (!Reflect.setPrototypeOf(native, null)) return false;
  if (Object.setPrototypeOf(native, target) !== native) return false;
  const child = Object.create(native);
  return Object.getPrototypeOf(child) === native && "toString" in child;
}
