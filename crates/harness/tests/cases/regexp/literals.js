// @returns undefined

import { expect } from 'test';

export function run() {
  function fresh() { return /ab+c/i; }
  const re = fresh();
  const captured = /(?<word>ab+c)(!)/i.exec("--ABBC!");
  const modes = /a/gimsy;
  expect(re.test("ABBC")).toBe(true);
  expect(re.test("AC")).toBe(false);
  expect(re !== fresh()).toBe(true);
  expect(re instanceof RegExp).toBe(true);
  expect(re.lastIndex).toBe(0);
  expect(re.source).toBe("ab+c");
  expect(re.flags).toBe("i");
  expect(re.toString()).toBe("/ab+c/i");
  expect(modes.flags).toBe("gimsy");
  expect(modes.global).toBe(true);
  expect(modes.ignoreCase).toBe(true);
  expect(modes.multiline).toBe(true);
  expect(modes.dotAll).toBe(true);
  expect(modes.sticky).toBe(true);
  expect(modes.unicode).toBe(false);
  expect(/^b/m.test("a\nb")).toBe(true);
  expect(/a.b/s.test("a\nb")).toBe(true);
  expect(Array.isArray(captured)).toBe(true);
  expect(captured.length).toBe(3);
  expect(captured[0]).toBe("ABBC!");
  expect(captured[1]).toBe("ABBC");
  expect(captured[2]).toBe("!");
  expect(captured.index).toBe(2);
  expect(captured.input).toBe("--ABBC!");
  expect(captured.groups.word).toBe("ABBC");
  expect(Object.getOwnPropertyDescriptor(captured, "index").value).toBe(2);
  expect(Object.getOwnPropertyDescriptor(captured, "input").value).toBe("--ABBC!");
  expect(Object.getOwnPropertyDescriptor(captured, "groups").value).toBe(captured.groups);
  expect(/a/.exec("a").groups).toBe(undefined);
}
