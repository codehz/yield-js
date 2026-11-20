// @returns undefined

import { expect } from 'test';

export function run() {
  const match = /./.exec("😀");
  expect(match !== null).toBe(true);
  expect(match[0].length).toBe(1);
  expect(match[0].charCodeAt(0)).toBe(0xD83D);
}
