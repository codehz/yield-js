// @returns undefined

import { expect } from 'test';
export function run() {
  let holes = new Array(3);
  expect(Array().length).toBe(0);
  expect(holes.length).toBe(3);
  expect(0 in holes).toBe(false);
  expect(Array.isArray(holes)).toBe(true);
  expect(Array.isArray(Array.prototype)).toBe(true);
  expect(Array.isArray({length: 3})).toBe(false);
  expect(Array.prototype.constructor).toBe(Array);
  expect(Object.getPrototypeOf([])).toBe(Array.prototype);
  expect(Object.getPrototypeOf(holes)).toBe(Array.prototype);
  expect(Array("3")[0]).toBe("3");
  expect(new Array(1, 2).join()).toBe("1,2");
  expect(Array.of(3).length).toBe(1);
  expect(0 in Array.from(holes)).toBe(true);
  expect(Array.from("a😀").join("|")).toBe("a|😀");
  let a = Array.from({0: 5, length: 2}, function(value, index) { return index === 0 ? value + this.delta : index; }, {delta: 2});
  expect(a.join()).toBe("7,1");
  let source = {[Symbol.iterator]() { return [4, 5].values(); }};
  expect(Array.from(source).join()).toBe("4,5");
  let negativeLengthError;
  try { Array(-1); } catch (e) { negativeLengthError = e; }
  expect(negativeLengthError instanceof RangeError).toBe(true);
  let fractionalLengthError;
  try { new Array(1.5); } catch (e) { fractionalLengthError = e; }
  expect(fractionalLengthError instanceof RangeError).toBe(true);
  let nullSourceError;
  try { Array.from(null); } catch (e) { nullSourceError = e; }
  expect(nullSourceError instanceof TypeError).toBe(true);
  let nullMapperError;
  try { Array.from([], null); } catch (e) { nullMapperError = e; }
  expect(nullMapperError instanceof TypeError).toBe(true);
}
