// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 2]
// @fulfilled true

import { expect, wait } from 'test';

function values(set) {
  const out = [];
  for (const value of set) out.push(value);
  return out;
}

export async function run() {
  const key = {};
  const groups = Map.groupBy([1, 2, 3], value => value === 2 ? key : 'odd');
  const union = new Set([3, 1]).union(new Map([[2, 0], [3, 0]]));
  const intersection = new Set([3, 1, 2]).intersection(new Map([[2, 0], [3, 0]]));
  const insert = Map.prototype.getOrInsert;
  const map = new Map([[key, 0]]);

  await wait('a');

  expect(Reflect.apply(insert, map, [key, 5])).toBe(0);
  expect(Reflect.apply(insert, map, ['fresh', 6])).toBe(6);
  expect(map.size).toBe(2);
  expect(groups.get(key)).toEqual([2]);
  expect(groups.get('odd')).toEqual([1, 3]);
  const order = [];
  for (const group of groups.keys()) order.push(group);
  expect(order[0]).toBe('odd');
  expect(order[1]).toBe(key);
  expect(values(union)).toEqual([3, 1, 2]);
  expect(values(intersection)).toEqual([2, 3]);
  const unionMethod = Set.prototype.union;
  const combined = Reflect.apply(unionMethod, new Set(['x']), [new Map([['y', 0]])]);
  expect(values(combined)).toEqual(['x', 'y']);
  return true;
}
