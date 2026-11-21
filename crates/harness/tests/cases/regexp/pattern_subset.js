// @returns undefined

import { expect } from 'test';

export function run() {
  // All four ECMAScript line terminators are independent, even CR followed by LF.
  const lines = /^.$/gm;
  const a = lines.exec("a\rb\u2028c\u2029d\r\ne");
  const b = lines.exec(a.input);
  const c = lines.exec(a.input);
  const d = lines.exec(a.input);
  const e = lines.exec(a.input);
  expect(a.index).toBe(0);
  expect(b.index).toBe(2);
  expect(c.index).toBe(4);
  expect(d.index).toBe(6);
  expect(e.index).toBe(9);
  expect(/a$/.test("a\n")).toBe(false);
  expect(/[a-z]/i.test("ſK")).toBe(false);
  expect(/\d/.test("١")).toBe(false);
  expect(/\s/.test("\uFEFF")).toBe(true);
  expect(/\s/.test("\u0085")).toBe(false);
  expect(/\bcat\b/.test("écaté")).toBe(true);
  expect(/[]/.test("")).toBe(false);
  expect(/[^]/.test("\n")).toBe(true);
  expect(/\uD83D\uDE00/.test("😀")).toBe(true);
}
