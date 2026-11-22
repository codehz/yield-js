// @returns undefined

import { expect } from 'test';

function throwsTypeError(callback) {
  try {
    callback();
  } catch (error) {
    expect(error instanceof TypeError).toBe(true);
    return;
  }
  expect(false).toBe(true);
}

export function run() {
  const sum = Math.sumPrecise;
  throwsTypeError(() => sum());
  throwsTypeError(() => sum(null));
  throwsTypeError(() => sum({0: 1, length: 1}));
  throwsTypeError(() => sum('1'));
  throwsTypeError(() => sum([1n]));
  throwsTypeError(() => sum([new Number(1)]));

  Number.prototype[Symbol.iterator] = function* () { yield 42; };
  try {
    expect(sum(7)).toBe(42);
  } finally {
    delete Number.prototype[Symbol.iterator];
  }

  let trace = '';
  let index = 0;
  const iterable = {
    [Symbol.iterator]() {
      trace += 'i';
      return {
        get next() {
          trace += 'g';
          return function () {
            trace += 'n';
            return { done: false, value: [NaN, Infinity, 'bad'][index++] };
          };
        },
        get return() {
          trace += 'r';
          return function () { trace += 'c'; throw 'close failure'; };
        }
      };
    }
  };
  throwsTypeError(() => sum(iterable));
  expect(trace).toBe('ignnnrc');

  for (const step of ['next', 'done', 'value']) {
    let closed = false;
    const marker = {};
    const input = {
      [Symbol.iterator]() {
        return {
          next() {
            if (step === 'next') throw marker;
            return {
              get done() { if (step === 'done') throw marker; return false; },
              get value() { throw marker; }
            };
          },
          return() { closed = true; return {}; }
        };
      }
    };
    let caught;
    try { sum(input); } catch (error) { caught = error; }
    expect(caught === marker).toBe(true);
    expect(closed).toBe(false);
  }

  const done = {
    [Symbol.iterator]() {
      return {
        next() { return { done: true, get value() { throw 'read after done'; } }; },
        return() { throw 'closed after done'; }
      };
    }
  };
  expect(Object.is(sum(done), -0)).toBe(true);
}
