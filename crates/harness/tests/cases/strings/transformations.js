// @returns undefined

import { expect } from 'test';

export function run() {
  expect("éß".toUpperCase()).toBe("ÉSS");
  expect("É".toLowerCase()).toBe("é");
  const spaced = "\uFEFF\t a \n";
  expect(spaced.trim()).toBe("a");
  expect(spaced.trimStart()).toBe("a \n");
  expect(spaced.trimEnd()).toBe("\uFEFF\t a");
  const parts = "a::b:c".split(":", 3);
  expect(parts.length).toBe(3);
  expect(parts[0]).toBe("a");
  expect(parts[1]).toBe("");
  expect(parts[2]).toBe("b");
  expect("ab".split(undefined)[0]).toBe("ab");
  expect("ab".split(":", 0).length).toBe(0);
  expect("ab".split("")[1]).toBe("b");
  expect("a".concat(2, null)).toBe("a2null");
  expect("é".repeat(2.9)).toBe("éé");
  expect("a".repeat(0)).toBe("");
  expect("é".padStart(4, "ab")).toBe("abaé");
  expect("é".padEnd(3)).toBe("é  ");
  expect("é".padEnd(4, "")).toBe("é");
  expect("abcabc".replace("b", "$$:$&:$`:$'")).toBe("a$:b:a:cabccabc");
  expect("😀x😀".replace("x", (match, index, input) => match + index + input.length)).toBe("😀x25😀");
  expect("abc".replace("x", () => { throw "unmatched"; })).toBe("abc");
  expect("abc".replaceAll("x", "long replacement")).toBe("abc");
  expect("x".padStart(2, "😀")).toBe("\uD83Dx");
  expect("x".padEnd(2, "😀")).toBe("x\uD83D");

  let repeatErrorName;
  try { "x".repeat(-1); } catch (error) { repeatErrorName = error.name; }
  expect(repeatErrorName).toBe("RangeError");
}
