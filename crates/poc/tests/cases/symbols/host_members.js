// @returns undefined

import { bind, expect } from 'test';

export function run() {
  const node = bind('alpha');
  const primitive = node[Symbol.toPrimitive];
  expect(typeof primitive).toBe('function');
  expect(primitive).toBe(node[Symbol.toPrimitive]);
  expect(primitive.call(node, 'string')).toBe('alpha');
  expect(primitive.call(node, 'number')).toBe('alpha');
}
