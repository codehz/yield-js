// @fulfilled undefined

import { expect } from 'test';

function rejects(action) {
  try { action(); } catch (error) { return error.name === "TypeError"; }
  return false;
}

export async function run() {
  const define = Object.defineProperty;
  const target = {};
  const read = function () { return 12; };
  define(target, "zero", { value: -0 });
  define(target, "nan", { value: 0 / 0 });
  define(target, "value", { value: 1, writable: true });
  define(target, "read", { get: read });
  define(target, "zero", { value: -0 });
  define(target, "nan", { value: 0 / 0 });
  define(target, "value", { value: 2, writable: false });
  define(target, "read", { get: read, set: undefined });
  expect(rejects(() => define(target, "zero", { value: 0 }))).toBe(true);
  expect(rejects(() => define(target, "value", { writable: true }))).toBe(true);
  expect(rejects(() => define(target, "value", { enumerable: true }))).toBe(true);
  expect(rejects(() => define(target, "value", { configurable: true }))).toBe(true);
  expect(rejects(() => define(target, "value", { get: read }))).toBe(true);
  expect(rejects(() => define(target, "read", { get: undefined }))).toBe(true);
  expect(rejects(() => define(target, "read", { value: undefined }))).toBe(true);
  expect(rejects(() => define(target, "bad", { get: read, value: undefined }))).toBe(true);
  expect(rejects(() => define(target, "bad", { set: null }))).toBe(true);
  expect(rejects(() => define(target, "bad", null))).toBe(true);
  expect(rejects(() => define(1, "bad", {}))).toBe(true);
  let descriptorError;
  try {
    define(target, "bad", { get value() { throw "descriptor"; } });
  } catch (error) {
    descriptorError = error;
  }
  expect(descriptorError).toBe("descriptor");
  expect(target.value).toBe(2);
  expect(target.read).toBe(12);
  expect("bad" in target).toBe(false);
}
