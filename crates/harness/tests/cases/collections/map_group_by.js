// @returns true

import { expect, gc } from 'test';

function typeError(f) {
  try {
    f();
  } catch (error) {
    return error instanceof TypeError;
  }
  return false;
}

function collect(iterator) {
  const values = [];
  for (const value of iterator) values.push(value);
  return values;
}

export function run() {
  const a = {};
  const b = {};
  const calls = [];
  const groups = Map.groupBy([1, 2, 3, 4], function (value, index) {
    expect(this).toBe(undefined);
    calls.push([value, index]);
    if (value === 1) return a;
    if (value === 2) return NaN;
    if (value === 3) return a;
    return b;
  });
  expect(groups instanceof Map).toBe(true);
  expect(Object.getPrototypeOf(groups)).toBe(Map.prototype);
  expect(groups.size).toBe(3);
  expect(calls.map(call => call[0])).toEqual([1, 2, 3, 4]);
  expect(calls.map(call => call[1])).toEqual([0, 1, 2, 3]);
  const keys = collect(groups.keys());
  expect(keys[0]).toBe(a);
  expect(Number.isNaN(keys[1])).toBe(true);
  expect(keys[2]).toBe(b);
  expect(groups.get(a)).toEqual([1, 3]);
  expect(groups.get(b)).toEqual([4]);
  expect(groups.get(NaN)).toEqual([2]);

  const odd = Map.groupBy([1, 2, 3], value => value % 2 === 1 ? 'odd' : 'even');
  expect(collect(odd.keys())).toEqual(['odd', 'even']);
  expect(odd.get('odd')).toEqual([1, 3]);
  expect(odd.get('even')).toEqual([2]);

  const zero = Map.groupBy([1], () => -0);
  expect(1 / zero.keys().next().value).toBe(Infinity);

  const symbol = Symbol('group');
  const bySymbol = Map.groupBy([5], () => symbol);
  expect(bySymbol.get(symbol)).toEqual([5]);

  expect(Map.groupBy('', () => 'unused').size).toBe(0);
  expect(Map.groupBy('ab', value => 'k' + value).get('ka')).toEqual(['a']);

  const marker = { marker: true };
  let closed = 0;
  const throwingIterator = {
    [Symbol.iterator]() {
      return {
        next() { return { value: 1, done: false }; },
        return() { closed++; throw 'return error'; }
      };
    }
  };
  let caught;
  try {
    Map.groupBy(throwingIterator, () => { throw marker; });
  } catch (error) {
    caught = error;
  }
  expect(caught).toBe(marker);
  expect(closed).toBe(1);

  let nextClosed = 0;
  const brokenNext = {
    [Symbol.iterator]() {
      return {
        next() { throw 'next error'; },
        return() { nextClosed++; return {}; }
      };
    }
  };
  caught = undefined;
  try {
    Map.groupBy(brokenNext, () => 'x');
  } catch (error) {
    caught = error;
  }
  expect(caught).toBe('next error');
  expect(nextClosed).toBe(0);

  // Partial groups stay reachable while the callback runs and calls gc().
  const collected = Map.groupBy([0, 1], function (value) {
    const key = { id: value };
    gc();
    return key;
  });
  expect(collected.size).toBe(2);
  expect(collected.keys().next().value.id).toBe(0);

  expect(typeError(() => Map.groupBy())).toBe(true);
  expect(typeError(() => Map.groupBy(null, () => 1))).toBe(true);
  expect(typeError(() => Map.groupBy([1], 1))).toBe(true);
  expect(typeError(() => Map.groupBy(1, () => 1))).toBe(true);
  expect(Map.groupBy.length).toBe(2);
  expect(Map.groupBy.name).toBe('groupBy');
  return true;
}
