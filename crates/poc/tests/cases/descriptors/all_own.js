// @returns undefined

import { expect } from 'test';

export function run() {
  const symbol = Symbol("hidden");
  const object = {};
  Object.defineProperty(object, "data", {
    value: 7, writable: false, enumerable: true, configurable: false,
  });
  Object.defineProperty(object, symbol, {
    get: Object.keys, enumerable: false, configurable: true,
  });
  const descriptors = Object.getOwnPropertyDescriptors(object);
  const data = descriptors.data;
  const accessor = descriptors[symbol];
  expect(Object.keys(descriptors).length).toBe(1);
  expect(data.value).toBe(7);
  expect(data.writable).toBe(false);
  expect(data.enumerable).toBe(true);
  expect(data.configurable).toBe(false);
  expect(accessor.get).toBe(Object.keys);
  expect(accessor.set).toBe(undefined);
  expect(accessor.enumerable).toBe(false);
  expect(accessor.configurable).toBe(true);
}
