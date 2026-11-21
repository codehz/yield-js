// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  const a = [1, 2];
  expect(a.values).toBe(a[Symbol.iterator]);
  expect(a.hasOwnProperty(Symbol.iterator)).toBe(false);
  const it = a.values();
  const other = a[Symbol.iterator]();
  expect(it[Symbol.iterator]()).toBe(it);
  const first = it.next();
  a[1] = 7;
  a[2] = 8;
  const second = it.next();
  const third = it.next();
  const independent = other.next();
  const end = it.next();
  a[3] = 9;
  const after = it.next();
  const again = it.next();
  const empty = [].values().next();

  const next = it.next;
  let badNext;
  try { next(); } catch (e) { badNext = e; }
  expect(typeof badNext).toBe("object");
  expect(badNext?.name).toBe("TypeError");
  expect(badNext?.message).toBe("ArrayIterator.next requires an array iterator");
  const borrowed = { values: a.values, length: 1, 0: 42 };
  const borrowedValue = borrowed.values().next();
  const bareValues = a.values;
  let badValues;
  try { bareValues(); } catch (e) { badValues = e; }
  expect(typeof badValues).toBe("object");
  expect(badValues?.name).toBe("TypeError");

  it.next = 42;
  a[Symbol.iterator] = 43;
  expect(first.value).toBe(1);
  expect(first.done).toBe(false);
  expect(second.value).toBe(7);
  expect(second.done).toBe(false);
  expect(third.value).toBe(8);
  expect(third.done).toBe(false);
  expect(independent.value).toBe(1);
  expect(independent.done).toBe(false);
  expect(end.done).toBe(true);
  expect(after === end).toBe(false);
  expect(again === after).toBe(false);
  expect(after.done).toBe(true);
  expect(again.done).toBe(true);
  expect(after.hasOwnProperty("value")).toBe(true);
  expect(after.value).toBe(undefined);
  expect(again.hasOwnProperty("value")).toBe(true);
  expect(again.value).toBe(undefined);
  expect(empty.done).toBe(true);
  expect(empty.hasOwnProperty("value")).toBe(true);
  expect(empty.value).toBe(undefined);
  expect(borrowedValue.value).toBe(42);
  expect(borrowedValue.done).toBe(false);
  expect(it.hasOwnProperty("next")).toBe(true);
  expect(it.next).toBe(42);
  expect(a.hasOwnProperty(Symbol.iterator)).toBe(true);
  expect(a[Symbol.iterator]).toBe(43);
}
