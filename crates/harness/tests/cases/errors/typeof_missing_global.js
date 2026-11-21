// @returns undefined

import { expect } from 'test';

export function run() {
  let count = 0;
  const side = () => { count += 1; return 4; };
  const records = [];
  function f(flag) {
    try {
      if (typeof Device == "object") return 0;
      if (flag) return Device;
      return 1;
    } finally {
      records.push("finally");
    }
  }

  expect(f(false)).toBe(1);
  let caught;
  try { f(true); } catch (error) { caught = error; }
  expect(caught instanceof ReferenceError).toBe(true);
  expect(caught.message).toBe("Device is not defined");
  expect(records.length).toBe(2);

  count = 0;
  try { Device(side()); } catch (error) { expect(error instanceof ReferenceError).toBe(true); }
  expect(count).toBe(0);
  try { Device?.(side()); } catch (error) { expect(error instanceof ReferenceError).toBe(true); }
  expect(count).toBe(0);
  try { Device += side(); } catch (error) { expect(error instanceof ReferenceError).toBe(true); }
  expect(count).toBe(0);
  try { Device = side(); } catch (error) { expect(error instanceof ReferenceError).toBe(true); }
  expect(count).toBe(1);
  count = 0;
  try { new Device(side()); } catch (error) { expect(error instanceof ReferenceError).toBe(true); }
  expect(count).toBe(0);
  try { Device ||= side(); } catch (error) { expect(error instanceof ReferenceError).toBe(true); }
  expect(count).toBe(0);
  expect(typeof Device || side()).toBe("undefined");
  expect(count).toBe(0);
  function shadow(Device) {
    if (typeof Device == "object") return Device.value;
    return 0;
  }
  expect(shadow({ value: 9 })).toBe(9);

  function* generator() { return Device; }
  const iterator = generator();
  let generatorError;
  try { iterator.next(); } catch (error) { generatorError = error; }
  expect(generatorError instanceof ReferenceError).toBe(true);
}
