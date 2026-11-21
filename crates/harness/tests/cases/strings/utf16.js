// @returns undefined

import { expect } from 'test';

export function run() {
  const value = "é😀z😀";
  expect(value.charAt(0)).toBe("é");
  expect(value.charAt(99)).toBe("");
  expect(value.charCodeAt(1)).toBe(55357);
  expect(Object.is(value.charCodeAt(-1), NaN)).toBe(true);
  expect(value.charCodeAt(2)).toBe(56832);
  expect(value.codePointAt(1)).toBe(128512);
  expect(value.codePointAt(2)).toBe(56832);
  expect(value.codePointAt(-1)).toBe(undefined);
  expect(value.at(-3)).toBe("z");
  expect(value.at(6)).toBe(undefined);
  expect(value.indexOf("😀", 2)).toBe(4);
  expect(value.lastIndexOf("😀", 3)).toBe(1);
  expect(value.lastIndexOf("😀")).toBe(4);
  expect(value.includes("😀", 2)).toBe(true);
  expect(value.includes("é", 1)).toBe(false);
  expect(value.indexOf("x")).toBe(-1);
  expect(value.startsWith("😀", 1)).toBe(true);
  expect(value.startsWith("😀")).toBe(false);
  expect(value.endsWith("😀", 3)).toBe(true);
  expect(value.endsWith("z")).toBe(false);
  expect(value.slice(-5, -3)).toBe("😀");
  expect(value.slice(3, 1)).toBe("");
  expect(value.substring(3, 1)).toBe("😀");
  expect(value.substring(-2, 1)).toBe("é");
  const high = "\uD83D";
  const low = "\uDE00";
  const units = value.split("");
  expect(value[1]).toBe(high);
  expect(value[6]).toBe(undefined);
  expect(value.charAt(1)).toBe(high);
  expect(value.at(-1)).toBe(low);
  expect(value.slice(0, 2)).toBe("é" + high);
  expect(value.substring(1, 2)).toBe(high);
  expect(units.length).toBe(6);
  expect(units[1]).toBe(high);
  expect(units[2]).toBe(low);
  expect(units.join("")).toBe(value);
}
