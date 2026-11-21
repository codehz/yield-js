// @returns undefined

import { expect } from 'test';

export function run() {
  expect(String.fromCharCode(233, 55357, 56832, 65536 + 122)).toBe("é😀z");
  expect(String.fromCodePoint(233, 128512, 122)).toBe("é😀z");
  expect(String.fromCharCode()).toBe("");
  expect(String.fromCodePoint()).toBe("");
  expect(String.raw({raw: ["a\\n", "b", "c"]}, 7)).toBe("a\\n7bc");
  expect(String.raw({raw: {0: "a", 1: 3, length: 2}}, false, "ignored")).toBe("afalse3");
  expect(String.raw({raw: []})).toBe("");
  expect(String.fromCharCode(55357)).toBe("\uD83D");
  expect(String.fromCodePoint(56832)).toBe("\uDE00");

  let outOfRangeErrorName;
  try { String.fromCodePoint(1114112); } catch (error) { outOfRangeErrorName = error.name; }
  expect(outOfRangeErrorName).toBe("RangeError");
  let fractionalErrorName;
  try { String.fromCodePoint(1.5); } catch (error) { fractionalErrorName = error.name; }
  expect(fractionalErrorName).toBe("RangeError");
  let rawErrorName;
  try { String.raw({}); } catch (error) { rawErrorName = error.name; }
  expect(rawErrorName).toBe("TypeError");
}
