// @returns undefined

import { expect } from 'test';

export function run() {
  const { pair: [first, second = 2] } = { pair: [1] };
  const values = [];
  const box = { total: 0 };
  for (const [left, right] of [[first, second], [3, 4]]) {
    values.push(left);
    box.total += right;
  }
  values[0]++;
  expect(first).toBe(1);
  expect(second).toBe(2);
  expect(values.length).toBe(2);
  expect(values[0]).toBe(2);
  expect(values[1]).toBe(3);
  expect(box.total).toBe(6);
}
