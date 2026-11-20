// @returns undefined

import { expect } from 'test';

export function run() {
  expect((12.5).toString()).toBe("12.5");
  expect((12.5).toString(10)).toBe("12.5");
  expect((12.5).toLocaleString()).toBe("12.5");
  expect((10).toString(2)).toBe("1010");
  expect((-31.75).toString("16")).toBe("-1f");
  expect((35).toString(36)).toBe("z");
  expect((-0).toString()).toBe("0");
  expect((-0).toString(2)).toBe("0");
  expect(NaN.toString(16)).toBe("NaN");
  expect(Infinity.toString()).toBe("Infinity");
  expect((-Infinity).toString(2)).toBe("-Infinity");
  expect((1.5).toFixed(2)).toBe("1.50");
  expect((2.75).toFixed()).toBe("3");
  expect((-0).toFixed(2)).toBe("0.00");
  expect((0).toFixed(100).length).toBe(102);
  expect((125).toExponential()).toBe("1.25e+2");
  expect((0.00125).toExponential(3)).toBe("1.250e-3");
  expect((-0).toExponential(1)).toBe("0.0e+0");
  expect((125).toPrecision()).toBe("125");
  expect((1.25).toPrecision(4)).toBe("1.250");
  expect((12500).toPrecision(3)).toBe("1.25e+4");
  expect((0.000000125).toPrecision(3)).toBe("1.25e-7");
  expect((-0).toPrecision(3)).toBe("0.00");
  expect(Infinity.toFixed(2)).toBe("Infinity");
  expect((-Infinity).toExponential(2)).toBe("-Infinity");
  expect(NaN.toPrecision(2)).toBe("NaN");
  expect(NaN.toExponential(101)).toBe("NaN");
  expect(Infinity.toPrecision(0)).toBe("Infinity");
  let smallRadixError;
  try {
    (1).toString(1);
  } catch (caught) {
    smallRadixError = caught;
  }
  expect(smallRadixError instanceof RangeError).toBe(true);
  let largeRadixError;
  try {
    (1).toString(37);
  } catch (caught) {
    largeRadixError = caught;
  }
  expect(largeRadixError instanceof RangeError).toBe(true);
  let negativeFixedDigitsError;
  try {
    (1).toFixed(-1);
  } catch (caught) {
    negativeFixedDigitsError = caught;
  }
  expect(negativeFixedDigitsError instanceof RangeError).toBe(true);
  let largeFixedDigitsError;
  try {
    (1).toFixed(101);
  } catch (caught) {
    largeFixedDigitsError = caught;
  }
  expect(largeFixedDigitsError instanceof RangeError).toBe(true);
  let negativeExponentialDigitsError;
  try {
    (1).toExponential(-1);
  } catch (caught) {
    negativeExponentialDigitsError = caught;
  }
  expect(negativeExponentialDigitsError instanceof RangeError).toBe(true);
  let largeExponentialDigitsError;
  try {
    (1).toExponential(101);
  } catch (caught) {
    largeExponentialDigitsError = caught;
  }
  expect(largeExponentialDigitsError instanceof RangeError).toBe(true);
  let zeroPrecisionError;
  try {
    (1).toPrecision(0);
  } catch (caught) {
    zeroPrecisionError = caught;
  }
  expect(zeroPrecisionError instanceof RangeError).toBe(true);
  let largePrecisionError;
  try {
    (1).toPrecision(101);
  } catch (caught) {
    largePrecisionError = caught;
  }
  expect(largePrecisionError instanceof RangeError).toBe(true);
  let receiverError;
  try {
    Reflect.apply(Number.prototype.toFixed, "1", [2]);
  } catch (caught) {
    receiverError = caught;
  }
  expect(receiverError instanceof TypeError).toBe(true);
}
