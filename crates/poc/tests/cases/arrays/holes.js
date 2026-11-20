// @returns undefined

import { expect } from 'test';

export function run() {
  let a = [1, , 2];
  expect(a.length).toBe(3);
  expect(1 in a).toBe(false);
  expect(a[1]).toBe(undefined);
  expect(a.map((value) => value === undefined ? 0 : value * 10).length).toBe(3);
  expect(a.map((value, index) => index in a).join()).toBe("true,,true");
  let seen = 0;
  a.forEach(() => { seen += 1; });
  expect(seen).toBe(2);
  expect(JSON.stringify(a)).toBe("[1,null,2]");
  expect(a.join()).toBe("1,,2");
}
