// @returns true

import { expect } from 'test';

function typeError(f) {
  try {
    f();
  } catch (error) {
    return error instanceof TypeError;
  }
  return false;
}

export function run() {
  let calls = 0;
  const m = new Map([['a', undefined]]);
  expect(m.getOrInsert('a', 1)).toBe(undefined);
  expect(m.getOrInsertComputed('a', function () {
    calls++;
    return 9;
  })).toBe(undefined);
  expect(calls).toBe(0);
  expect(typeError(() => m.getOrInsertComputed('a', 1))).toBe(true);
  expect(m.size).toBe(1);

  expect(m.getOrInsert('b', 2)).toBe(2);
  expect(m.getOrInsert('b', 3)).toBe(2);
  expect(m.get('b')).toBe(2);

  let received;
  const n = new Map();
  expect(n.getOrInsertComputed(-0, function (key) {
    received = key;
    return 7;
  })).toBe(7);
  expect(1 / received).toBe(Infinity);
  expect(1 / n.keys().next().value).toBe(Infinity);

  const p = new Map();
  expect(p.getOrInsertComputed('k', function (key) {
    expect(this).toBe(undefined);
    expect(key).toBe('k');
    p.set('k', 1);
    p.set('other', 2);
    return 3;
  })).toBe(3);
  expect(p.get('k')).toBe(3);
  expect(p.size).toBe(2);
  expect(p.keys().next().value).toBe('k');

  const marker = { marker: true };
  const q = new Map();
  try {
    q.getOrInsertComputed('x', function () {
      q.set('side', 1);
      throw marker;
    });
  } catch (caught) {
    expect(caught).toBe(marker);
  }
  expect(q.get('side')).toBe(1);
  expect(q.has('x')).toBe(false);

  expect(typeError(() => Reflect.apply(Map.prototype.getOrInsert, new Set(), ['a', 1]))).toBe(true);
  expect(typeError(() => Reflect.apply(Map.prototype.getOrInsertComputed, new Set(), ['a', function () {}]))).toBe(true);
  expect(Map.prototype.getOrInsert.length).toBe(2);
  expect(Map.prototype.getOrInsertComputed.length).toBe(2);
  expect(Map.prototype.getOrInsert.name).toBe('getOrInsert');
  expect(Map.prototype.getOrInsertComputed.name).toBe('getOrInsertComputed');
  const descriptor = Object.getOwnPropertyDescriptor(Map.prototype, 'getOrInsert');
  expect(descriptor.writable).toBe(true);
  expect(descriptor.enumerable).toBe(false);
  expect(descriptor.configurable).toBe(true);
  return true;
}
