// @returns undefined

import { expect } from 'test';

function typeError(fn) {
  let error;
  try { fn(); } catch (e) { error = e; }
  expect(error instanceof TypeError).toBe(true);
}

export function run() {
  let log = '';
  let joined;
  function iterable(label, values, reenter) {
    const input = {
      get [Symbol.iterator]() {
        log += 'get ' + label + ';';
        return function () {
          expect(this).toBe(input);
          expect(arguments.length).toBe(0);
          log += 'open ' + label + ';';
          if (reenter) typeError(() => joined.next());
          let index = 0;
          const inner = {
            get next() {
              log += 'get next ' + label + ';';
              return function () {
                expect(this).toBe(inner);
                expect(arguments.length).toBe(0);
                log += 'next ' + label + ';';
                if (reenter) typeError(() => joined.return());
                if (index < values.length) return { value: values[index++], done: false };
                return { done: true, get value() { throw new Error('completed iterator value read'); } };
              };
            },
            get return() {
              log += 'get return ' + label + ';';
              return function () {
                expect(this).toBe(inner);
                expect(arguments.length).toBe(0);
                log += 'close ' + label + ';';
                if (reenter) {
                  typeError(() => joined.next());
                  typeError(() => joined.return());
                }
                return { value: 'ignored', done: false };
              };
            }
          };
          return inner;
        };
      }
    };
    return input;
  }

  const a = iterable('a', [1], true);
  const empty = iterable('empty', [], false);
  const b = iterable('b', [2, 3], true);
  const c = iterable('c', [4], false);
  joined = Iterator.concat(a, empty, b, c);
  expect(joined instanceof Iterator).toBe(true);
  expect(joined[Symbol.iterator]()).toBe(joined);
  expect(Object.prototype.toString.call(joined)).toBe('[object Iterator Helper]');
  expect(log).toBe('get a;get empty;get b;get c;');
  Object.defineProperty(b, Symbol.iterator, {
    value() { throw new Error('open method was replaced'); }
  });
  expect(joined.next('ignored').value).toBe(1);
  expect(log).toBe('get a;get empty;get b;get c;open a;get next a;next a;');
  expect(joined.next().value).toBe(2);
  const opened = 'get a;get empty;get b;get c;open a;get next a;next a;next a;' +
    'open empty;get next empty;next empty;open b;get next b;next b;';
  expect(log).toBe(opened);
  const closed = joined.return('ignored');
  expect(closed.value).toBe(undefined);
  expect(closed.done).toBe(true);
  expect(joined.return().done).toBe(true);
  expect(joined.next().done).toBe(true);
  expect(log).toBe(opened + 'get return b;close b;');

  log = '';
  const unopened = Iterator.concat(a, c);
  expect(unopened.return().done).toBe(true);
  expect(unopened.next().done).toBe(true);
  expect(log).toBe('get a;get c;');
  const none = Iterator.concat();
  expect(none.next().done).toBe(true);
  expect(none.return().done).toBe(true);
  expect(Iterator.concat([5], [6, 7]).toArray().join()).toBe('5,6,7');

  log = '';
  typeError(() => Iterator.concat(a, 'text', c));
  expect(log).toBe('get a;');
  typeError(() => Iterator.concat({ next() { return { done: true }; } }));
  typeError(() => Iterator.concat({ [Symbol.iterator]: 1 }));
  const invalidOpen = Iterator.concat({ [Symbol.iterator]() { return 0; } });
  typeError(() => invalidOpen.next());
  expect(invalidOpen.next().done).toBe(true);

  // Unlike the wrapper's return, IteratorClose rejects a primitive result.
  const invalidClose = Iterator.concat({
    [Symbol.iterator]() {
      return Iterator.from({ next() { return { value: 8 }; }, return() { return 0; } });
    }
  });
  expect(invalidClose.next().value).toBe(8);
  typeError(() => invalidClose.return());
  expect(invalidClose.next().done).toBe(true);
}
