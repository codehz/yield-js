// @returns undefined

import { expect } from 'test';

function syntaxError(pattern, flags) {
  try {
    new RegExp(pattern, flags);
    return false;
  } catch (error) {
    return error.name === "SyntaxError";
  }
}

export function run() {
  const re = new RegExp({ toString() { return "ab+c"; } }, {
    toString() { return "ig"; },
  });
  const copy = new RegExp(re);
  const override = RegExp(re, "y");
  expect(re.flags).toBe("gi");
  expect(re.test("ABBC")).toBe(true);
  expect(re.lastIndex).toBe(4);
  expect(RegExp(re)).toBe(re);
  expect(copy !== re).toBe(true);
  expect(copy.source).toBe(re.source);
  expect(copy.flags).toBe(re.flags);
  expect(copy.lastIndex).toBe(0);
  expect(override !== re).toBe(true);
  expect(override.source).toBe(re.source);
  expect(override.flags).toBe("y");
  expect(RegExp("abc").test("abc")).toBe(true);
  expect(syntaxError("[", "")).toBe(true);
  expect(syntaxError("abc", "gg")).toBe(true);
  expect(syntaxError("", "uv")).toBe(true);
}
