// @returns true

import { expect } from 'test';

export function run() {
  const a = expect({ n: 1 });
  const b = expect({ n: 2 });
  if (a.toBe !== b.toBe) return false;
  if (a.toEqual !== b.toEqual) return false;

  const bare = a.toBe;
  try {
    bare(1);
    return false;
  } catch (error) {
    if (!(error instanceof TypeError)) return false;
  }

  const fake = Object.create(Object.getPrototypeOf(a));
  try {
    fake.toBe(1);
    return false;
  } catch (error) {
    if (!(error instanceof TypeError)) return false;
  }

  return typeof a.toBe === 'function' && typeof a.label === 'undefined';
}
