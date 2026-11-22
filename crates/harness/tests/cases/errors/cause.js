// @returns undefined

import { expect } from 'test';

export function run() {
  const reason = {};
  const inherited = Object.create({ cause: reason });
  for (const Ctor of [Error, EvalError, RangeError, ReferenceError, SyntaxError, TypeError, URIError]) {
    const descriptor = Object.getOwnPropertyDescriptor(new Ctor(undefined, inherited), 'cause');
    expect(descriptor.value).toBe(reason);
    expect(descriptor.writable).toBe(true);
    expect(descriptor.enumerable).toBe(false);
    expect(descriptor.configurable).toBe(true);
    expect(Object.hasOwn(Ctor('', { cause: undefined }), 'cause')).toBe(true);
  }
  expect(Object.hasOwn(Error('', {}), 'cause')).toBe(false);
  expect(Object.hasOwn(Error('', 'primitive'), 'cause')).toBe(false);
  expect(Object.hasOwn(new SuppressedError(reason, reason, '', inherited), 'cause')).toBe(false);

  let order = '';
  const options = new Proxy({}, {
    has(target, key) { order += 'has:' + key + ';'; return true; },
    get(target, key) { order += 'get:' + key + ';'; return reason; },
  });
  const errors = {
    [Symbol.iterator]() { order += 'iterator;'; return [3][Symbol.iterator](); },
  };
  const aggregate = AggregateError(errors, {
    toString() { order += 'message;'; return 'aggregate'; },
  }, options);
  expect(order).toBe('message;has:cause;get:cause;iterator;');
  expect(aggregate.cause).toBe(reason);
  expect(aggregate.errors[0]).toBe(3);

  let thrown;
  try { Error('', { get cause() { throw reason; } }); } catch (error) { thrown = error; }
  expect(thrown).toBe(reason);
}
