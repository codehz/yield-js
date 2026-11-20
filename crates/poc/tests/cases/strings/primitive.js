// @returns undefined

import { expect } from 'test';

export function run() {
  const value = String("é😀z");
  expect(typeof value).toBe("string");
  expect(String()).toBe("");
  expect(String(undefined)).toBe("undefined");
  let symbolErrorName;
  try { String(Symbol("tag")); } catch (error) { symbolErrorName = error.name; }
  expect(symbolErrorName).toBe("TypeError");
  expect(value.length).toBe(4);
  expect(value[0]).toBe("é");
  expect(value["3"]).toBe("z");
  expect(value[4]).toBe(undefined);
  expect(value[-1]).toBe(undefined);
  expect(value["03"]).toBe(undefined);
  expect(value.missing).toBe(undefined);
  expect(value.constructor).toBe(String);
  expect(value.toString()).toBe(value);
  expect(value.valueOf()).toBe(value);
  expect(value[Symbol.iterator]).toBe(String.prototype[Symbol.iterator]);

  let lengthErrorName;
  try { value.length = 0; } catch (error) { lengthErrorName = error.name; }
  expect(lengthErrorName).toBe("TypeError");
  let indexErrorName;
  try { value[0] = "x"; } catch (error) { indexErrorName = error.name; }
  expect(indexErrorName).toBe("TypeError");
  let extraErrorName;
  try { value.extra = true; } catch (error) { extraErrorName = error.name; }
  expect(extraErrorName).toBe("TypeError");
  expect(value.length).toBe(4);
  expect(value[0]).toBe("é");
  expect(value.extra).toBe(undefined);
}
