// @returns undefined

import { expect } from 'test';

export function run() {
  const global = /foo/g;
  const first = global.exec("-foo-foo");
  expect(first.index).toBe(1);
  expect(global.lastIndex).toBe(4);
  expect(global.test("-foo-foo")).toBe(true);
  expect(global.lastIndex).toBe(8);
  expect(global.exec("-foo-foo")).toBe(null);
  expect(global.lastIndex).toBe(0);

  const sticky = /foo/y;
  expect(sticky.test("-foo")).toBe(false);
  expect(sticky.lastIndex).toBe(0);
  sticky.lastIndex = 1;
  expect(sticky.exec("-foo").index).toBe(1);
  expect(sticky.lastIndex).toBe(4);
  expect(sticky.test("-foo")).toBe(false);
  expect(sticky.lastIndex).toBe(0);

  const empty = /a*/g;
  const zero = empty.exec("b");
  expect(zero[0]).toBe("");
  expect(zero.index).toBe(0);
  expect(empty.lastIndex).toBe(0);
  expect(empty.exec("b").index).toBe(0);
  expect(empty.lastIndex).toBe(0);
  expect(empty.test("b")).toBe(true);
  expect(empty.test("b")).toBe(true);
  expect(empty.lastIndex).toBe(0);

  const frozen = /a/g;
  Object.defineProperty(frozen, "lastIndex", { writable: false });
  let frozenError;
  try {
    frozen.exec("a");
  } catch (error) {
    frozenError = error;
  }
  expect(frozenError instanceof TypeError).toBe(true);

  const sentinel = {};
  const ordinary = /a/;
  ordinary.lastIndex = { valueOf() { throw sentinel; } };
  let coercionError;
  try {
    ordinary.exec("a");
  } catch (error) {
    coercionError = error;
  }
  expect(coercionError).toBe(sentinel);

  const utf16 = /foo/g;
  expect(utf16.exec("😀foo").index).toBe(2);
  expect(utf16.lastIndex).toBe(5);

  const plain = /foo/;
  const desc = Object.getOwnPropertyDescriptor(plain, "lastIndex");
  plain.lastIndex = 99;
  expect(desc.value).toBe(0);
  expect(desc.writable).toBe(true);
  expect(desc.enumerable).toBe(false);
  expect(desc.configurable).toBe(false);
  expect(plain.exec("-foo").index).toBe(1);
  expect(plain.lastIndex).toBe(99);
  expect(plain.test("other")).toBe(false);
  expect(plain.lastIndex).toBe(99);
}
