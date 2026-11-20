// @returns undefined

import { expect } from 'test';

export function run() {
  expect(Number.parseInt("\u00a0\t-0XfFtail")).toBe(-255);
  expect(Number.parseInt(" +015rest", 0)).toBe(15);
  expect(Number.parseInt("0x10", 16)).toBe(16);
  expect(Number.parseInt("0x10", 10)).toBe(0);
  expect(Number.parseInt("1012", "2")).toBe(5);
  expect(Number.parseInt("Zz!", 36)).toBe(1295);
  expect(Number.parseInt("11", 4294967298)).toBe(3);
  expect(Number.parseInt("-11", 2.9)).toBe(-3);
  expect(Object.is(Number.parseInt("-0junk"), -0)).toBe(true);
  expect(Number.parseInt(42)).toBe(42);
  expect(Number.isNaN(Number.parseInt("0x"))).toBe(true);
  expect(Number.isNaN(Number.parseInt("xyz", 10))).toBe(true);
  expect(Number.isNaN(Number.parseInt("1", 1))).toBe(true);
  expect(Number.isNaN(Number.parseInt("1", 37))).toBe(true);
  expect(Number.parseFloat("\u00a0\t-.125e+2tail")).toBe(-12.5);
  expect(Number.parseFloat(" +.5x")).toBe(0.5);
  expect(Number.parseFloat("1e+oops")).toBe(1);
  expect(Number.parseFloat("0x10")).toBe(0);
  expect(Number.parseFloat("+Infinitytail")).toBe(Infinity);
  expect(Number.parseFloat("-Infinity!") !== -Infinity).toBe(false);
  expect(Object.is(Number.parseFloat("-0.00rest"), -0)).toBe(true);
  expect(Number.parseFloat({toString() { return "8.5tail"; }})).toBe(8.5);
  expect(Number.isNaN(Number.parseFloat("."))).toBe(true);
  expect(Number.isNaN(Number.parseFloat("  "))).toBe(true);
  expect(Number.isNaN(Number.parseFloat("infinity"))).toBe(true);
  let parseIntError;
  try {
    Number.parseInt(Symbol());
  } catch (caught) {
    parseIntError = caught;
  }
  expect(parseIntError instanceof TypeError).toBe(true);
  let parseFloatError;
  try {
    Number.parseFloat(Symbol());
  } catch (caught) {
    parseFloatError = caught;
  }
  expect(parseFloatError instanceof TypeError).toBe(true);
}
