// @returns undefined

import { expect } from 'test';
export function run() {
  let a = Array(3);
  a[1] = 4;
  expect(a.push(7, 8)).toBe(5);
  expect(a.pop()).toBe(8);
  expect(a.shift()).toBe(undefined);
  expect(a.length).toBe(3);
  expect(a[0]).toBe(4);
  expect(1 in a).toBe(false);
  expect(a[2]).toBe(7);
  expect(a.unshift(1, 2)).toBe(5);
  expect(a.join()).toBe("1,2,4,,7");
  let removed = a.splice(-3, 2, 9);
  expect(removed.length).toBe(2);
  expect(removed[0]).toBe(4);
  expect(1 in removed).toBe(false);
  expect(a.join()).toBe("1,2,9,7");
  a.splice(1, 0, 6, 5);
  expect(a.join()).toBe("1,6,5,2,9,7");
  expect(a.slice(-4, -1).join()).toBe("5,2,9");
  expect(a.splice(4).join()).toBe("9,7");
  expect(a.splice().length).toBe(0);
  expect(a.length).toBe(4);
  expect(Array(4).slice(1).length).toBe(3);
  expect(0 in Array(2).slice()).toBe(false);
  let filled = Array(3).fill(5);
  expect(filled.join()).toBe("5,5,5");
  expect(0 in filled).toBe(true);
  let frozen = Object.freeze([1, 2]);
  let shiftError;
  try { frozen.shift(); } catch (e) { shiftError = e; }
  expect(shiftError instanceof TypeError).toBe(true);
  let popError;
  try { frozen.pop(); } catch (e) { popError = e; }
  expect(popError instanceof TypeError).toBe(true);
  let locked = [];
  Object.defineProperty(locked, "length", {writable: false});
  let pushError;
  try { locked.push(1); } catch (e) { pushError = e; }
  expect(pushError instanceof TypeError).toBe(true);
  let unshiftError;
  try { locked.unshift(); } catch (e) { unshiftError = e; }
  expect(unshiftError instanceof TypeError).toBe(true);
  expect(frozen.join()).toBe("1,2");
  expect(locked.length).toBe(0);
}
