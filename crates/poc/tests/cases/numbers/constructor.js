// @returns undefined

import { expect } from 'test';

export function run() {
  expect(Object.is(Number(), 0)).toBe(true);
  expect(Object.is(Number(" -0 "), -0)).toBe(true);
  expect(Number(null)).toBe(0);
  expect(Number(true)).toBe(1);
  expect(Number(false)).toBe(0);
  expect(Number(" 0x20 ")).toBe(32);
  expect(Number({valueOf() { return "3.5"; }})).toBe(3.5);
  expect(Number.isNaN(Number(undefined))).toBe(true);
  expect(Number.isNaN(Number("2xyz"))).toBe(true);
  expect(Number.prototype.constructor).toBe(Number);
  expect(Object.getPrototypeOf(Number.prototype)).toBe(Object.prototype);
  expect((1) instanceof Number).toBe(false);
  expect(Object.is(Number.prototype, 0)).toBe(false);
  expect(Object.create(Number.prototype) instanceof Number).toBe(true);
  expect((1).missing).toBe(undefined);
  expect((2).valueOf()).toBe(2);
  expect(Object.is((-0).valueOf(), -0)).toBe(true);
  let n = 4;
  Number.prototype.extra = "shared";
  expect(n.extra).toBe("shared");
  expect(n.toString).toBe(Number.prototype.toString);
  Object.defineProperty(Number.prototype, "doubled", {get() { return this.valueOf() * 2; }});
  expect(n.doubled).toBe(8);
  expect(n["constructor"]).toBe(Number);
  let symbolError;
  try {
    Number(Symbol());
  } catch (caught) {
    symbolError = caught;
  }
  expect(symbolError instanceof TypeError).toBe(true);
  let sharedPropertyError;
  try {
    n.extra = "changed";
  } catch (caught) {
    sharedPropertyError = caught;
  }
  expect(sharedPropertyError instanceof TypeError).toBe(true);
  let newPropertyError;
  try {
    n.other = 0;
  } catch (caught) {
    newPropertyError = caught;
  }
  expect(newPropertyError instanceof TypeError).toBe(true);
  let receiverError;
  try {
    Number.prototype.valueOf();
  } catch (caught) {
    receiverError = caught;
  }
  expect(receiverError instanceof TypeError).toBe(true);
  expect(n.extra).toBe("shared");
  expect(n.other).toBe(undefined);
}
