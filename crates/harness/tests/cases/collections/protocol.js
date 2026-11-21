// @returns undefined

import { expect } from 'test';

export function run() {
  let closed = 0;
  const iterable = {
    [Symbol.iterator]() {
      let index = 0;
      return {
        next() {
          index++;
          return index === 1 ? { value: { get 0() { return 'k'; }, 1: 2 }, done: false } : { done: true };
        },
        return() { closed++; return {}; }
      };
    }
  };
  const m = new Map(iterable);
  expect(m.get('k')).toBe(2);
  expect(closed).toBe(0);
  expect(new Map(null).size).toBe(0);
  expect(new Set(undefined).size).toBe(0);
  expect(new Set('a😀a').size).toBe(2);
  expect(new Map(m).get('k')).toBe(2);
  function typeError(f) {
    try { f(); } catch (e) { return e instanceof TypeError; }
    return false;
  }
  const s = new Set();
  const ms = Object.getOwnPropertyDescriptor(Map.prototype, 'size');
  const ss = Object.getOwnPropertyDescriptor(Set.prototype, 'size');
  expect(ms.enumerable).toBe(false);
  expect(ss.enumerable).toBe(false);
  expect(ms.configurable).toBe(true);
  expect(ms.set).toBe(undefined);
  expect(Reflect.apply(ms.get, m, [])).toBe(1);
  expect(Reflect.apply(ss.get, s, [])).toBe(0);
  const bad = {
    [Symbol.iterator]() {
      return {
        next() { return { value: 'x', done: false }; },
        return() { closed++; throw 'preserve the earlier error'; }
      };
    }
  };
  expect(typeError(() => Map())).toBe(true);
  expect(typeError(() => Set())).toBe(true);
  expect(typeError(() => new Map(3))).toBe(true);
  expect(typeError(() => new Set({}))).toBe(true);
  expect(typeError(() => new Map(bad))).toBe(true);
  expect(closed).toBe(1);
  expect(typeError(() => Map.prototype.size)).toBe(true);
  expect(typeError(() => Set.prototype.size)).toBe(true);
  expect(typeError(() => Reflect.apply(ms.get, s, []))).toBe(true);
  expect(typeError(() => Reflect.apply(ss.get, m, []))).toBe(true);
  expect(typeError(() => Reflect.apply(Map.prototype.get, s, ['k']))).toBe(true);
  expect(typeError(() => Reflect.apply(Set.prototype.add, m, [1]))).toBe(true);
  expect(typeError(() => Reflect.apply(m.keys().next, s.keys(), []))).toBe(true);
  expect(typeError(() => s.forEach(1))).toBe(true);
}
