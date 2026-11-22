// @returns "ok"

import { expect, gc } from 'test';

export function run() {
  const matchAll = RegExp.prototype[Symbol.matchAll];
  const split = RegExp.prototype[Symbol.split];
  const descriptor = Object.getOwnPropertyDescriptor(RegExp, Symbol.species);
  expect(descriptor.enumerable).toBe(false);
  expect(descriptor.configurable).toBe(true);
  expect(descriptor.set).toBe(undefined);
  expect(RegExp[Symbol.species]).toBe(RegExp);
  class Derived extends RegExp {
    exec(input) { return { input, derived: true }; }
  }
  expect(Derived[Symbol.species]).toBe(Derived);
  const derived = matchAll.call(new Derived('x'), 'custom');
  expect(derived.next().value.derived).toBe(true);
  expect(derived.next().done).toBe(true);

  const direct = /a/[Symbol.matchAll]('baa');
  expect(direct.next().value.index).toBe(1);
  expect(direct.next().done).toBe(true);
  expect(/z/[Symbol.matchAll]('aaa').next().done).toBe(true);

  let trace = '';
  let calls = 0;
  const receiver = {
    get constructor() {
      trace += 'constructor ';
      return {
        get [Symbol.species]() {
          trace += 'species ';
          gc();
          return function (pattern, flags) {
            trace += 'construct ';
            expect(pattern).toBe(receiver);
            expect(flags).toBe('y');
            return {
              set lastIndex(value) { gc(); expect(value).toBe(2); },
              exec(input) {
                gc();
                expect(input).toBe('abc');
                calls++;
                return { get 0() { throw 'non-global matchAll must not read 0'; } };
              },
            };
          };
        },
      };
    },
    get flags() { trace += 'flags '; return 'y'; },
    get lastIndex() {
      trace += 'lastIndex ';
      return { valueOf() { gc(); return 2.9; } };
    },
  };
  const iterator = matchAll.call(receiver, { toString() { trace += 'input '; return 'abc'; } });
  expect(trace).toBe('input constructor species flags construct lastIndex ');
  expect(calls).toBe(0);
  gc();
  expect(iterator.next().done).toBe(false);
  expect(iterator.next().done).toBe(true);
  expect(calls).toBe(1);

  const empty = /ignored/gu;
  const seen = [];
  empty.constructor = {
    [Symbol.species]: function () {
      return {
        exec(input) {
          seen.push(this.lastIndex);
          return this.lastIndex <= input.length ? { 0: '' } : null;
        },
      };
    },
  };
  const global = matchAll.call(empty, '😀');
  expect(global.next().done).toBe(false);
  expect(global.next().done).toBe(false);
  expect(global.next().done).toBe(true);
  expect(seen.join(',')).toBe('0,2,3');

  const separator = /ignored/u;
  separator.lastIndex = 17;
  let built = 0;
  let captureReads = 0;
  let execCalls = 0;
  separator.constructor = {
    [Symbol.species]: function (pattern, flags) {
      expect(pattern).toBe(separator);
      expect(flags).toBe('uy');
      built++;
      return {
        exec(input) {
          execCalls++;
          if (input[this.lastIndex] !== ',') return null;
          this.lastIndex++;
          return {
            get index() { throw 'split must not read index'; },
            get 0() { throw 'split must not read 0'; },
            get length() { gc(); return '2.9'; },
            get 1() { gc(); return ++captureReads; },
          };
        },
      };
    },
  };
  expect(split.call(separator, '😀,b,c', { valueOf() { gc(); expect(built).toBe(1); return 4; } }).join('|'))
    .toBe('😀|1|b|2');
  expect(execCalls).toBe(4); // The initial astral character is advanced as one code point.
  expect(separator.lastIndex).toBe(17);
  expect(split.call(separator, '', 0).length).toBe(0);
  expect(built).toBe(2);
  expect(execCalls).toBe(4);
  expect(split.call(separator, '').length).toBe(1);

  const fallback = /,/;
  fallback.constructor = { [Symbol.species]: null };
  expect(split.call(fallback, 'x,y').join('|')).toBe('x|y');
  expect(matchAll.call(fallback, 'x,y').next().value.index).toBe(1);
  for (const invalid of [null, { [Symbol.species]: () => ({}) }]) {
    for (const method of [matchAll, split]) {
      let threw = false;
      try { method.call({ constructor: invalid, get flags() { throw 'flags'; } }, ''); }
      catch (error) { threw = error instanceof TypeError; }
      expect(threw).toBe(true);
    }
  }

  const badExec = /a/;
  badExec.constructor = { [Symbol.species]: function () { return { exec() { return false; } }; } };
  for (const action of [() => matchAll.call(badExec, 'a').next(), () => split.call(badExec, '')]) {
    let threw = false;
    try { action(); } catch (error) { threw = error instanceof TypeError; }
    expect(threw).toBe(true);
  }
  return 'ok';
}
