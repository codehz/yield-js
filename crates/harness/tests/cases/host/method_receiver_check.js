// @returns undefined

import { expect } from 'test';

export function run() {
  const a = expect({ n: 1 });
  const b = expect({ n: 2 });
  expect(a.toBe).toBe(b.toBe);
  expect(a.toEqual).toBe(b.toEqual);

  const bare = a.toBe;
  let bareError;
  try {
    bare(1);
  } catch (error) {
    bareError = error;
  }
  expect(bareError instanceof TypeError).toBe(true);

  const fake = Object.create(Object.getPrototypeOf(a));
  let fakeError;
  try {
    fake.toBe(1);
  } catch (error) {
    fakeError = error;
  }
  expect(fakeError instanceof TypeError).toBe(true);

  expect(typeof a.toBe).toBe('function');
  expect(typeof a.label).toBe('undefined');
}
