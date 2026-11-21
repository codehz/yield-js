// @returns undefined

import { expect } from 'test';

export function run() {
  let order = '';
  const thrown = {};
  const arrow = () => {};
  const parent = {
    get prototype() {
      order += 'g';
      if (this !== arrow) return {};
      throw thrown;
    }
  };
  Object.setPrototypeOf(arrow, parent);
  const bound = Function.prototype.bind.call(arrow, null);
  expect(bound.bind).toBe(undefined);
  expect(1 instanceof bound).toBe(false);
  expect(order).toBe('');

  function left() { order += 'l'; return {}; }
  function right() { order += 'r'; return bound; }
  let error;
  try {
    left() instanceof right();
  } catch (caught) {
    error = caught;
  }
  expect(error).toBe(thrown);
  expect(order).toBe('lrg');
}
