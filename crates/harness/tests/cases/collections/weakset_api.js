// @returns undefined

import { expect } from 'test';

export function run() {
  const object = {};
  const bare = Symbol();
  const first = Symbol("s");
  const second = Symbol("s");
  const registered = Symbol.for("s");
  const set = new WeakSet([object, bare, first, Symbol.iterator]);
  expect(Object.getPrototypeOf(set)).toBe(WeakSet.prototype);
  expect(set.constructor).toBe(WeakSet);
  set.extra = 7;
  expect(set.extra).toBe(7);
  expect(set.has(object)).toBe(true);
  expect(set.has({})).toBe(false);
  expect(set.has(bare)).toBe(true);
  expect(set.has(first)).toBe(true);
  expect(set.has(second)).toBe(false);
  expect(set.has(Symbol.iterator)).toBe(true);
  expect(set.add(second)).toBe(set);
  expect(set.add(second)).toBe(set);
  expect(set.delete(second)).toBe(true);
  expect(set.delete(second)).toBe(false);
  expect(set.has(first)).toBe(true);
  expect(set.size).toBe(undefined);
  expect(set.clear).toBe(undefined);
  expect(set[Symbol.iterator]).toBe(undefined);
  expect(new WeakSet(null).has(object)).toBe(false);

  function throwsTypeError(f) {
    try { f(); } catch (e) { return e instanceof TypeError; }
    return false;
  }
  for (const invalid of [null, undefined, true, 1, "x", registered]) {
    expect(set.has(invalid)).toBe(false);
    expect(set.delete(invalid)).toBe(false);
    expect(throwsTypeError(() => set.add(invalid))).toBe(true);
  }
  expect(set.has()).toBe(false);
  expect(set.delete()).toBe(false);
  expect(throwsTypeError(() => set.add())).toBe(true);
  expect(throwsTypeError(() => WeakSet())).toBe(true);
  expect(throwsTypeError(() => new WeakSet([object, 1]))).toBe(true);
  expect(throwsTypeError(() => new WeakSet([registered]))).toBe(true);
  expect(throwsTypeError(() => WeakSet.prototype.has.call(new WeakMap()))).toBe(true);
  expect(throwsTypeError(() => WeakSet.prototype.add.call(new Set(), object))).toBe(true);
}
