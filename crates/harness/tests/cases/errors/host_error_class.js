// @returns true

import { expect, allocError, lazyThrow, ParseError } from 'test';

const token = { marker: 'cause-token' };

function check(instance, name, message, cause) {
  expect(Error.isError(instance)).toBe(true);
  expect(instance instanceof Error).toBe(true);
  expect(instance instanceof ParseError).toBe(true);
  expect(instance.name).toBe(name);
  expect(Object.keys(instance).includes('message')).toBe(false);
  expect(Object.keys(instance).includes('stack')).toBe(false);
  expect(instance.stack.startsWith(name + ': ' + message)).toBe(true);
  expect(instance.cause === cause).toBe(true);
}

export function run() {
  const allocated = allocError('from CallCx');
  check(allocated, 'ParseError', 'from CallCx', undefined);

  // The lazy variant is allocated at the host return boundary with the
  // active stack, so its stack includes the JS call position.
  let lazy;
  try { lazyThrow(); } catch (error) { lazy = error; }
  check(lazy, 'ParseError', 'lazy parse failure', undefined);
  expect(lazy.stack.includes('    at ')).toBe(true);
  expect(lazy.stack.includes('(main.js:')).toBe(true);

  const constructed = new ParseError('bad', { cause: token });
  check(constructed, 'ParseError', 'bad', token);

  // Calling without `new` still allocates a branded instance.
  const plain = ParseError('plain');
  check(plain, 'ParseError', 'plain', undefined);

  class Child extends ParseError {}
  const child = new Child('child message');
  expect(child instanceof Child).toBe(true);
  expect(child instanceof ParseError).toBe(true);
  expect(child instanceof Error).toBe(true);
  expect(child.name).toBe('ParseError');
  expect(Object.getPrototypeOf(Child.prototype) === ParseError.prototype).toBe(true);
  expect(Child.prototype.constructor === Child).toBe(true);

  // The declared constructor is module-scoped, never a global.
  expect('ParseError' in globalThis).toBe(false);
  expect(ParseError.prototype.constructor === ParseError).toBe(true);
  expect(Object.getPrototypeOf(ParseError) === Error).toBe(true);
  expect(ParseError.length).toBe(1);
  expect(ParseError.name).toBe('ParseError');

  // Subclass instances are branded plain Errors without host payloads.
  expect(child.stack.startsWith('ParseError: child message')).toBe(true);

  return true;
}
