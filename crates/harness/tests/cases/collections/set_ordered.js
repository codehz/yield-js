// @returns undefined

import { expect } from 'test';

export function run() {
  const obj = {};
  const s = new Set([NaN, NaN, -0, 0, obj, obj]);
  expect(s.size).toBe(3);
  expect(s.has(NaN)).toBe(true);
  expect(s.has(-0)).toBe(true);
  expect(s.has(obj)).toBe(true);
  expect(s.has({})).toBe(false);
  expect(s.add(obj)).toBe(s);
  expect(s.size).toBe(3);
  expect(s.delete(NaN)).toBe(true);
  expect(s.delete(NaN)).toBe(false);
  expect(s.keys).toBe(s.values);
  expect(s[Symbol.iterator]).toBe(s.values);
  expect(Set.prototype.constructor).toBe(Set);
  expect(Object.getPrototypeOf(Set.prototype)).toBe(Object.prototype);
  const keys = Reflect.ownKeys(Set.prototype);
  expect(keys.slice(0, -2).join(',')).toBe('constructor,add,has,delete,clear,size,forEach,values,keys,entries,union,intersection,difference,symmetricDifference,isSubsetOf,isSupersetOf,isDisjointFrom');
  expect(keys[keys.length - 2]).toBe(Symbol.iterator);
  expect(keys[keys.length - 1]).toBe(Symbol.toStringTag);
  const e = s.entries();
  expect(e[Symbol.iterator]()).toBe(e);
  const zero = e.next().value;
  expect(1 / zero[0]).toBe(Infinity);
  expect(zero[1] === 0).toBe(true);
  const dup = e.next().value;
  expect(dup[0]).toBe(obj);
  expect(dup[1]).toBe(obj);
  expect(s.clear()).toBe(undefined);
  expect(s.size).toBe(0);
  s.add('a').add('b');
  expect(e.next().value[0]).toBe('a');
  const ctx = { log: '' };
  s.forEach(function (v, k, self) {
    expect(k).toBe(v);
    expect(self).toBe(s);
    this.log += v;
    if (v === 'a') {
      s.clear();
      s.add('c');
    }
  }, ctx);
  expect(ctx.log).toBe('ac');
  expect(e.next().value[0]).toBe('c');
  expect(e.next().done).toBe(true);
  expect(s.size).toBe(1);
}
