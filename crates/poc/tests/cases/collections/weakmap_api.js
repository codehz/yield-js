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
  const bare = Symbol();
  const first = Symbol("s");
  const second = Symbol("s");
  const registered = Symbol.for("s");
  expect(map.set(bare, 1)).toBe(map);
  map.set(first, 2);
  expect(map.has(second)).toBe(false);
  map.set(second, 3);
  map.set(Symbol.iterator, 4);
  expect(map.get(bare)).toBe(1);
  expect(map.get(first)).toBe(2);
  expect(map.get(second)).toBe(3);
  expect(map.get(Symbol.iterator)).toBe(4);
  expect(map.get(key)).toBe(value);
  const invalidKeys = [null, undefined, true, 1, "x", registered];
  for (const invalid of invalidKeys) {
    expect(map.get(invalid)).toBe(undefined);
    expect(map.has(invalid)).toBe(false);
    expect(throwsTypeError(() => map.set(invalid, value))).toBe(true);
    expect(map.delete(invalid)).toBe(false);
  }
  expect(map.get()).toBe(undefined);
  expect(map.has()).toBe(false);
  expect(map.delete()).toBe(false);
  expect(throwsTypeError(() => map.set())).toBe(true);
  expect(throwsTypeError(() => new WeakMap([[registered, value]]))).toBe(true);
  expect(throwsTypeError(() => WeakMap.prototype.get.call(new WeakSet()))).toBe(true);
  expect(throwsTypeError(() => WeakMap.prototype.set.call(new Map(), key, value))).toBe(true);
}
