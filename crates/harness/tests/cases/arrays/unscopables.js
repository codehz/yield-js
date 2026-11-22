// @returns undefined

import { expect } from 'test';

export function run() {
  const key = Symbol.unscopables;
  expect(typeof key).toBe("symbol");
  expect(key.description).toBe("Symbol.unscopables");
  const global = Object.getOwnPropertyDescriptor(Symbol, "unscopables");
  expect(global.value).toBe(key);
  expect(global.writable || global.enumerable || global.configurable).toBe(false);

  const descriptor = Object.getOwnPropertyDescriptor(Array.prototype, key);
  const list = descriptor.value;
  expect(descriptor.writable || descriptor.enumerable).toBe(false);
  expect(descriptor.configurable).toBe(true);
  expect([][key]).toBe(list);
  expect(Object.getPrototypeOf(list)).toBe(null);
  expect(Reflect.ownKeys(list).join(",")).toBe(
    "at,copyWithin,entries,fill,find,findIndex,findLast,findLastIndex,flat,flatMap,includes,keys,toReversed,toSorted,toSpliced,values"
  );
  for (const name of Reflect.ownKeys(list)) {
    const entry = Object.getOwnPropertyDescriptor(list, name);
    expect(entry.value).toBe(true);
    expect(entry.writable && entry.enumerable && entry.configurable).toBe(true);
  }
}
