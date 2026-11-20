// @returns undefined

import { expect } from 'test';

export function run() {
  let repeatError;
  try { "x".repeat(-1); } catch (error) { repeatError = error; }
  expect(repeatError instanceof RangeError).toBe(true);
  expect(repeatError.name).toBe("RangeError");

  let codePointError;
  try { String.fromCodePoint(0x110000); } catch (error) { codePointError = error; }
  expect(codePointError instanceof RangeError).toBe(true);
  expect(codePointError.name).toBe("RangeError");
}
