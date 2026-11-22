// @returns true

import { expect, gc } from 'test';

function values(set) {
  const out = [];
  for (const value of set) out.push(value);
  return out;
}

function errorName(f) {
  try {
    f();
  } catch (error) {
    return error.name;
  }
  return 'none';
}

function typeError(f) {
  return errorName(f) === 'TypeError';
}

function emptyKeys() {
  return [][Symbol.iterator]();
}

export function run() {
  const log = [];
  const ordered = {
    get size() { log.push('size'); return 1; },
    get has() { log.push('has'); return function () { log.push('has call'); return false; }; },
    get keys() { log.push('keys'); return function () { log.push('keys call'); return emptyKeys(); }; }
  };
  expect(new Set([1, 2]).isSubsetOf(ordered)).toBe(false);
  expect(log).toEqual(['size', 'has', 'keys']);

  expect(errorName(() => new Set().union({ size: NaN, has() {}, keys() {} }))).toBe('TypeError');
  expect(errorName(() => new Set().union({ size: undefined, has() {}, keys() {} }))).toBe('TypeError');
  expect(errorName(() => new Set().union({ size: -1, has() {}, keys() {} }))).toBe('RangeError');
  expect(errorName(() => new Set().union({ size: -Infinity, has() {}, keys() {} }))).toBe('RangeError');
  expect(errorName(() => new Set().union({ size: Symbol('size'), has() {}, keys() {} }))).toBe('TypeError');
  expect(errorName(() => new Set().union({ size: 0, has: 1, keys() {} }))).toBe('TypeError');
  expect(errorName(() => new Set().union({ size: 0, has() {}, keys: 1 }))).toBe('TypeError');
  expect(errorName(() => new Set().union({ size: -0.5, has() { return false; }, keys: emptyKeys }))).toBe('none');
  expect(new Set([1]).isSubsetOf({ size: '1', has() { return true; }, keys: emptyKeys })).toBe(true);
  expect(new Set([1]).union({ size: Infinity, has() { return false; }, keys: emptyKeys }).size).toBe(1);

  const live = new Set([1, 2, 3]);
  const mutatingOther = {
    size: 3,
    has(value) {
      if (value === 1) {
        live.delete(2);
        live.add(4);
      }
      return true;
    },
    keys: emptyKeys
  };
  expect(values(live.intersection(mutatingOther))).toEqual([1, 3, 4]);

  const snapshot = new Set([1, 2]);
  const mutatingOnHas = {
    size: 2,
    has(value) {
      if (value === 1) {
        snapshot.delete(2);
        snapshot.add(3);
      }
      return value === 1;
    },
    keys: emptyKeys
  };
  expect(values(snapshot.difference(mutatingOnHas))).toEqual([2]);

  const receiver = new Set(['a']);
  const mutatingOnKeys = {
    size: 0,
    has() { return false; },
    keys() {
      receiver.add('b');
      return new Set(['c']).keys();
    }
  };
  expect(values(receiver.union(mutatingOnKeys))).toEqual(['a', 'b', 'c']);

  const rooted = new Set([1]);
  const gcOther = {
    get size() { return 1; },
    get has() { return function () { return true; }; },
    get keys() {
      gc();
      const keys = new Set(['9']);
      return function () { return keys.keys(); };
    }
  };
  expect(rooted.isSubsetOf(gcOther)).toBe(true);

  let closed = 0;
  const sentinel = { close: true };
  const closing = {
    size: 0,
    has() { return false; },
    keys() {
      return {
        next() { return { value: 'missing', done: false }; },
        return() { closed++; throw sentinel; }
      };
    }
  };
  let caught;
  try {
    new Set(['a']).isSupersetOf(closing);
  } catch (error) {
    caught = error;
  }
  expect(caught).toBe(sentinel);
  expect(closed).toBe(1);

  let exhaustedClosed = 0;
  const exhausted = {
    size: 0,
    has() { return false; },
    keys() {
      return {
        next() { return { done: true }; },
        return() { exhaustedClosed++; return {}; }
      };
    }
  };
  expect(new Set(['a']).isSupersetOf(exhausted)).toBe(true);
  expect(exhaustedClosed).toBe(0);

  let reads = 0;
  const probe = {
    get size() { reads++; return 0; },
    get has() { reads++; return function () {}; },
    get keys() { reads++; return emptyKeys; }
  };
  expect(typeError(() => Reflect.apply(Set.prototype.union, new Map(), [probe]))).toBe(true);
  expect(reads).toBe(0);
  return true;
}
