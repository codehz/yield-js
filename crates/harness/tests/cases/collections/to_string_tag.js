// @returns true

import { expect } from 'test';

function checkTag(target, value) {
  const descriptor = Object.getOwnPropertyDescriptor(target, Symbol.toStringTag);
  expect(descriptor.value).toBe(value);
  expect(descriptor.writable).toBe(false);
  expect(descriptor.enumerable).toBe(false);
  expect(descriptor.configurable).toBe(true);
}

export function run() {
  const m = new Map([[1, 2]]);
  const s = new Set([1]);
  const mi = m.keys();
  const si = s.values();
  expect(Object.getOwnPropertyDescriptor(m, Symbol.toStringTag)).toBe(undefined);
  expect(Object.prototype.toString.call(m)).toBe('[object Map]');
  expect(Object.prototype.toString.call(s)).toBe('[object Set]');
  expect(Object.prototype.toString.call(mi)).toBe('[object Map Iterator]');
  expect(Object.prototype.toString.call(si)).toBe('[object Set Iterator]');
  expect(Object.prototype.toString.call(Map.prototype)).toBe('[object Map]');
  expect(Object.prototype.toString.call(Set.prototype)).toBe('[object Set]');

  checkTag(Map.prototype, 'Map');
  checkTag(Set.prototype, 'Set');
  checkTag(Object.getPrototypeOf(mi), 'Map Iterator');
  checkTag(Object.getPrototypeOf(si), 'Set Iterator');

  Object.defineProperty(m, Symbol.toStringTag, { value: 'Custom' });
  expect(Object.prototype.toString.call(m)).toBe('[object Custom]');
  expect(Object.prototype.toString.call(s)).toBe('[object Set]');

  const numeric = new Map();
  Object.defineProperty(numeric, Symbol.toStringTag, { value: 1 });
  expect(Object.prototype.toString.call(numeric)).toBe('[object Map]');
  return true;
}
