// @returns undefined

import { expect } from 'test';

export function run() {
  expect(globalThis.SuppressedError).toBe(SuppressedError);
  expect(SuppressedError.length).toBe(3);
  expect(Object.getPrototypeOf(SuppressedError)).toBe(Error);
  expect(Object.getPrototypeOf(SuppressedError.prototype)).toBe(Error.prototype);
  expect(SuppressedError.prototype.constructor).toBe(SuppressedError);

  const error = { toString() { throw "unexpected error coercion"; } };
  const suppressed = { toString() { throw "unexpected suppressed coercion"; } };
  const options = { get cause() { throw "unexpected cause access"; } };
  let conversions = 0;
  const message = { toString() { conversions++; return "details"; } };
  const called = SuppressedError(error, suppressed, message, options);
  expect(called instanceof SuppressedError).toBe(true);
  expect(called instanceof Error).toBe(true);
  expect(called.name).toBe("SuppressedError");
  expect(called.message).toBe("details");
  expect(conversions).toBe(1);
  expect(called.hasOwnProperty("cause")).toBe(false);
  for (const [key, value] of [["error", error], ["suppressed", suppressed]]) {
    const descriptor = Object.getOwnPropertyDescriptor(called, key);
    expect(descriptor.value).toBe(value);
    expect(descriptor.writable).toBe(true);
    expect(descriptor.configurable).toBe(true);
    expect(descriptor.enumerable).toBe(false);
  }

  const empty = new SuppressedError();
  expect(Object.getPrototypeOf(empty)).toBe(SuppressedError.prototype);
  expect(empty.hasOwnProperty("message")).toBe(false);
  expect(empty.message).toBe("");
  expect(empty.hasOwnProperty("error")).toBe(true);
  expect(empty.hasOwnProperty("suppressed")).toBe(true);
  expect(empty.error).toBe(undefined);
  expect(empty.suppressed).toBe(undefined);
  expect(SuppressedError(error, suppressed, undefined, options).hasOwnProperty("message")).toBe(false);
  expect(new SuppressedError(error, suppressed, null).message).toBe("null");

  class Derived extends SuppressedError {}
  const derived = new Derived(error, suppressed, "derived");
  expect(Object.getPrototypeOf(derived)).toBe(Derived.prototype);
  expect(derived instanceof SuppressedError).toBe(true);
  expect(derived.error).toBe(error);
  expect(derived.suppressed).toBe(suppressed);
  expect(derived.message).toBe("derived");

  let thrown;
  try {
    SuppressedError(error, suppressed, { toString() { throw error; } }, options);
  } catch (value) {
    thrown = value;
  }
  expect(thrown).toBe(error);
}
