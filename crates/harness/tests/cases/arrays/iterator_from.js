// @returns undefined

import { expect } from 'test';

function typeError(fn) {
  let error;
  try { fn(); } catch (e) { error = e; }
  expect(error instanceof TypeError).toBe(true);
}

export function run() {
  const arrayIterator = [1, 2].values();
  expect(Iterator.from(arrayIterator)).toBe(arrayIterator);
  let reads = 0;
  const inherited = Object.create(Iterator.prototype, {
    next: { get() { reads++; return undefined; } }
  });
  const iterable = {
    [Symbol.iterator]() {
      expect(this).toBe(iterable);
      return inherited;
    },
    get next() { throw new Error('wrong iterator'); }
  };
  expect(Iterator.from(iterable)).toBe(inherited);
  expect(reads).toBe(1);
  expect(Iterator.from('A💡').toArray().join('|')).toBe('A|💡');
  typeError(() => Iterator.from(42));
  typeError(() => Iterator.from({ [Symbol.iterator]() { return 42; } }));

  let log = '';
  let close;
  const result = { value: 'identity', done: true };
  const source = {
    [Symbol.iterator]: null,
    get next() {
      log += 'get next;';
      let count = 0;
      return function () {
        expect(this).toBe(source);
        expect(arguments.length).toBe(0);
        log += 'next;';
        return count++ === 0 ? result : 23;
      };
    },
    get return() { log += 'get return;'; return close; }
  };
  const wrapped = Iterator.from(source);
  expect(wrapped === source).toBe(false);
  expect(wrapped instanceof Iterator).toBe(true);
  expect(wrapped[Symbol.iterator]()).toBe(wrapped);
  expect(log).toBe('get next;');
  Object.defineProperty(source, 'next', { value() { throw new Error('next was replaced'); } });
  expect(wrapped.next('ignored')).toBe(result);
  expect(wrapped.next('ignored')).toBe(23);
  expect(log).toBe('get next;next;next;');

  const absent = wrapped.return('ignored');
  expect(absent.done).toBe(true);
  expect(absent.value).toBe(undefined);
  close = function () {
    expect(this).toBe(source);
    expect(arguments.length).toBe(0);
    log += 'close;';
    return 37;
  };
  expect(wrapped.return('ignored')).toBe(37);
  close = function () { log += 'new close;'; return result; };
  expect(wrapped.return()).toBe(result);
  close = 0;
  typeError(() => wrapped.return());
  expect(wrapped.next()).toBe(23);
  expect(log).toBe('get next;next;next;get return;get return;close;get return;new close;get return;next;');

  const fake = Object.create(Object.getPrototypeOf(wrapped));
  typeError(() => wrapped.next.call(fake));
  typeError(() => wrapped.return.call(fake));
  const missingNext = Iterator.from({});
  typeError(() => missingNext.next());
}
