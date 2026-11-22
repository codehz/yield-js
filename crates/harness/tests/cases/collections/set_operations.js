// @returns true

import { expect } from 'test';

function values(set) {
  const out = [];
  for (const value of set) out.push(value);
  return out;
}

function typeError(f) {
  try {
    f();
  } catch (error) {
    return error instanceof TypeError;
  }
  return false;
}

export function run() {
  const base = new Set([3, 1, 2]);
  const other = new Map([[2, 'x'], [3, 'y']]);
  expect(values(base.union(other))).toEqual([3, 1, 2]);
  expect(values(base.intersection(other))).toEqual([2, 3]);
  expect(values(base.difference(other))).toEqual([1]);
  expect(values(base.symmetricDifference(other))).toEqual([1]);
  expect(base.isSubsetOf(other)).toBe(false);
  expect(base.isSupersetOf(other)).toBe(true);
  expect(base.isDisjointFrom(other)).toBe(false);
  expect(values(base)).toEqual([3, 1, 2]);
  expect(other.size).toBe(2);

  const left = new Set(['b', 'a']);
  const right = new Map([['a', 1], ['b', 2]]);
  expect(values(left.intersection(right))).toEqual(['b', 'a']);

  expect(values(new Set([1]).intersection(new Set()))).toEqual([]);
  expect(values(new Set().union(new Set([1, 2])))).toEqual([1, 2]);
  expect(new Set([1]).isDisjointFrom(new Set())).toBe(true);
  expect(new Set().isSubsetOf(new Set())).toBe(true);
  expect(new Set().isSupersetOf(new Set())).toBe(true);

  const duplicates = {
    size: 0,
    has() { return false; },
    keys() { return ['a', 'a'][Symbol.iterator](); }
  };
  expect(values(new Set(['a']).union(duplicates))).toEqual(['a']);
  expect(values(new Set(['a']).symmetricDifference(duplicates))).toEqual([]);

  const extras = {
    size: 0,
    has() { return false; },
    keys() { return ['b', 'b'][Symbol.iterator](); }
  };
  expect(values(new Set(['a']).union(extras))).toEqual(['a', 'b']);

  const nan = new Set([NaN, -0]);
  const numeric = new Map([[NaN, 1], [0, 2]]);
  expect(nan.union(numeric).size).toBe(2);
  expect(nan.symmetricDifference(numeric).size).toBe(0);
  expect(nan.isSubsetOf(numeric)).toBe(true);
  expect(nan.isSupersetOf(numeric)).toBe(true);
  expect(nan.isDisjointFrom(numeric)).toBe(false);

  const symbol = Symbol('member');
  expect(values(new Set([symbol]).union(new Set([symbol, 1])))).toEqual([symbol, 1]);

  const baseAddFree = new Set([1, 2]);
  const otherFree = new Map([[2, 0], [3, 0]]);
  const originalAdd = Set.prototype.add;
  Set.prototype.add = function () { throw 'add called'; };
  try {
    expect(values(baseAddFree.union(otherFree))).toEqual([1, 2, 3]);
    expect(values(baseAddFree.intersection(otherFree))).toEqual([2]);
  } finally {
    Set.prototype.add = originalAdd;
  }

  const unionResult = new Set([1]).union(new Set([2]));
  expect(Object.getPrototypeOf(unionResult)).toBe(Set.prototype);
  unionResult.add(99);
  expect(new Set([1]).union(new Set([2])).size).toBe(2);

  const names = ['union', 'intersection', 'difference', 'symmetricDifference', 'isSubsetOf', 'isSupersetOf', 'isDisjointFrom'];
  for (const name of names) {
    const method = Set.prototype[name];
    expect(typeof method).toBe('function');
    expect(method.length).toBe(1);
    expect(method.name).toBe(name);
    const descriptor = Object.getOwnPropertyDescriptor(Set.prototype, name);
    expect(descriptor.writable).toBe(true);
    expect(descriptor.enumerable).toBe(false);
    expect(descriptor.configurable).toBe(true);
  }

  expect(typeError(() => Reflect.apply(Set.prototype.union, new Map(), [new Set()]))).toBe(true);
  expect(typeError(() => Reflect.apply(Set.prototype.isSubsetOf, {}, [new Set()]))).toBe(true);
  expect(typeError(() => base.union(1))).toBe(true);
  expect(typeError(() => base.intersection(null))).toBe(true);
  expect(typeError(() => base.union())).toBe(true);
  return true;
}
