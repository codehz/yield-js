// @returns undefined

import { expect } from 'test';
export function run() {
  let a = Array(4);
  a[1] = 3;
  a[3] = 5;
  let state = {sum: 0};
  a.forEach(function(value, index, source) { if (source === a) this.sum += value + index; }, state);
  expect(state.sum).toBe(12);
  let mapped = a.map((x, i) => x * i);
  expect(mapped.length).toBe(4);
  expect(0 in mapped).toBe(false);
  expect(mapped[1]).toBe(3);
  expect(2 in mapped).toBe(false);
  expect(mapped[3]).toBe(15);
  expect(a.filter((x) => x > 3).join()).toBe("5");
  expect(a.reduce((acc, x, i) => acc + x + i, 1)).toBe(13);
  expect(a.reduce((acc, x) => acc + x)).toBe(8);
  expect(a.some((x) => x === 5)).toBe(true);
  expect(a.every((x) => x > 0)).toBe(true);
  expect(a.some(() => false)).toBe(false);
  expect(a.every((x) => x < 5)).toBe(false);
  let nested = [2, undefined, [3]];
  let outer = [1, undefined, nested];
  delete nested[1];
  delete outer[1];
  let flat = outer.flat(2);
  expect(flat.length).toBe(3);
  expect(flat.join()).toBe("1,2,3");
  expect(Array.isArray([[[4]]].flat()[0])).toBe(true);
  expect(a.indexOf(undefined)).toBe(-1);
  expect(a.includes(undefined)).toBe(true);
  expect([NaN].includes(NaN)).toBe(true);
  expect([2, 3, 2].lastIndexOf(2, -2)).toBe(0);
  expect(a.find((x) => x === 5)).toBe(5);
  expect(a.findIndex((x) => x === undefined)).toBe(0);
  expect(a.flatMap((x) => { let row = [x, undefined, x]; delete row[1]; return row; }).join()).toBe("3,3,5,5");
  expect(1 in [1].concat(a)).toBe(false);
  let reduceError;
  try { Array(2).reduce((acc, x) => x); } catch (e) { reduceError = e; }
  expect(reduceError instanceof TypeError).toBe(true);
  let mapError;
  try { a.map(() => { throw 42; }); } catch (e) { mapError = e; }
  expect(mapError).toBe(42);
  let closed = 0;
  let source = {[Symbol.iterator]() { return {next() { return {value: 1}; }, return() { closed++; throw 9; }}; }};
  let fromError;
  try { Array.from(source, () => { throw 7; }); } catch (e) { fromError = e; }
  expect(fromError).toBe(7);
  expect(closed).toBe(1);
}
