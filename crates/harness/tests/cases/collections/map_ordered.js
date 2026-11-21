// @returns undefined

import { expect } from 'test';

export function run() {
  const a = {};
  const b = {};
  const m = new Map([[a, 1], [b, 2], [NaN, 3], [-0, 4]]);
  expect(m.size).toBe(4);
  expect(m.get(a)).toBe(1);
  expect(m.has({})).toBe(false);
  expect(m.get(NaN)).toBe(3);
  expect(m.get(0)).toBe(4);
  expect(m.set(a, 5)).toBe(m);
  expect(m.get(a)).toBe(5);
  expect(m.keys().next().value).toBe(a);
  expect(m.delete(a)).toBe(true);
  expect(m.delete(a)).toBe(false);
  expect(m.get(a)).toBe(undefined);
  m.set(a, 6);
  expect(m.keys().next().value).toBe(b);
  expect(m.values().next().value).toBe(2);
  expect(m[Symbol.iterator]).toBe(m.entries);
  expect(Map.prototype.constructor).toBe(Map);
  expect(Object.getPrototypeOf(Map.prototype)).toBe(Object.prototype);
  const i = m.entries();
  expect(i[Symbol.iterator]()).toBe(i);
  expect(i.next().value[0]).toBe(b);
  m.set(NaN, 7);
  expect(i.next().value[1]).toBe(7);
  expect(1 / i.next().value[0]).toBe(Infinity);
  expect(m.clear()).toBe(undefined);
  expect(m.size).toBe(0);
  m.set('new', 8);
  expect(i.next().value[1]).toBe(8);
  expect(i.next().done).toBe(true);
  m.set('late', 9);
  expect(i.next().done).toBe(true);
  const ctx = { log: '' };
  m.forEach(function (v, k, self) {
    expect(self).toBe(m);
    this.log += k + v;
    if (k === 'new') {
      m.delete('late');
      m.set('end', 10);
    }
  }, ctx);
  expect(ctx.log).toBe('new8end10');
}
