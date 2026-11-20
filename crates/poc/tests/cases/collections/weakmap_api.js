// @returns undefined

import { expect } from 'test';

export function run() {
  const key = {};
  const other = {};
  const value = { marker: 7 };
  const map = new WeakMap([[key, value]]);
  expect(map.get(key)).toBe(value);
  expect(map.has(key)).toBe(true);
  expect(map.get(other)).toBe(undefined);
  expect(map.set(key, other)).toBe(map);
  expect(map.get(key)).toBe(other);
  expect(map.delete(key)).toBe(true);
  expect(map.delete(key)).toBe(false);
  expect(map.has(key)).toBe(false);
  map.set(key, value);
  expect(map[Symbol.iterator]).toBe(undefined);
  expect(map.size).toBe(undefined);
  function throwsTypeError(f) {
    try { f(); } catch (e) { return e instanceof TypeError; }
    return false;
  }
  expect(throwsTypeError(() => WeakMap())).toBe(true);
  expect(throwsTypeError(() => new WeakMap([[1, value]]))).toBe(true);
  const primitives = [null, undefined, true, 1, "x", Symbol("s"), Symbol.for("s")];
  for (const primitive of primitives) {
    expect(throwsTypeError(() => map.get(primitive))).toBe(true);
    expect(throwsTypeError(() => map.has(primitive))).toBe(true);
    expect(throwsTypeError(() => map.set(primitive, value))).toBe(true);
    expect(throwsTypeError(() => map.delete(primitive))).toBe(true);
  }
}
