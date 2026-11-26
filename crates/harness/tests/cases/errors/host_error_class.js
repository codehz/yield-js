// @returns true

import { expect, allocError, lazyThrow, ParseError, ErrorBase, ErrorDerived, HostDOMException, bind, gc } from 'test';

const token = { marker: 'cause-token' };

function check(instance, name, message, cause) {
  expect(Error.isError(instance)).toBe(true);
  expect(instance instanceof Error).toBe(true);
  expect(instance instanceof ParseError).toBe(true);
  expect(instance.name).toBe(name);
  expect(instance.message).toBe(message);
  expect(Object.keys(instance).includes('message')).toBe(false);
  expect(Object.keys(instance).includes('stack')).toBe(false);
  expect(instance.stack.startsWith(name + ': ' + message)).toBe(true);
  expect(instance.cause === cause).toBe(true);
}

function thrownBy(call) {
  try { call(); } catch (error) { return error; }
  throw new Error('expected a thrown value');
}

export function run() {
  const allocated = allocError('from CallCx');
  check(allocated, 'ParseError', 'from CallCx', undefined);

  // The host allocates before throwing, so the stack includes the JS call.
  let lazy;
  try { lazyThrow(); } catch (error) { lazy = error; }
  check(lazy, 'ParseError', 'lazy parse failure', undefined);
  expect(lazy.stack.includes('    at ')).toBe(true);
  expect(lazy.stack.includes('(main.js:')).toBe(true);

  const constructed = new ParseError('bad', { cause: token });
  check(constructed, 'ParseError', 'bad', token);

  expect(thrownBy(() => ParseError('plain')) instanceof TypeError).toBe(true);
  expect(Error.isError(Error('plain'))).toBe(true);

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

  expect(Error.isError(child)).toBe(true);
  expect(child.stack.startsWith('ParseError: child message')).toBe(true);

  class Grandchild extends ErrorDerived {}
  const derived = new Grandchild(token);
  expect(Error.isError(derived)).toBe(true);
  expect(derived instanceof ErrorBase).toBe(true);
  expect(derived instanceof ErrorDerived).toBe(true);
  expect(derived instanceof Grandchild).toBe(true);
  expect(derived.detail() === token).toBe(true);
  expect(derived.name).toBe('Error');
  expect(derived.message).toBe('');
  expect(Object.hasOwn(derived, 'message')).toBe(false);
  expect(Object.getPrototypeOf(ErrorDerived) === ErrorBase).toBe(true);

  const fake = Object.create(ParseError.prototype);
  expect(fake instanceof ParseError).toBe(true);
  expect(Error.isError(fake)).toBe(false);
  expect(Error.isError({ name: 'ParseError', message: 'bad' })).toBe(false);
  expect(Error.isError(new Proxy(constructed, {
    get() { throw token; },
    getPrototypeOf() { throw token; },
  }))).toBe(false);
  const ordinary = bind('not an error');
  Object.setPrototypeOf(ordinary, ParseError.prototype);
  expect(Error.isError(ordinary)).toBe(false);
  try { throw ordinary; } catch (error) { expect(error === ordinary).toBe(true); }
  Object.setPrototypeOf(constructed, null);
  expect(Error.isError(constructed)).toBe(true);
  expect(constructed instanceof ParseError).toBe(false);
  expect(thrownBy(() => structuredClone(constructed)).name).toBe('DataCloneError');

  const named = Object.getOwnPropertyDescriptor(ParseError.prototype, 'name');
  expect(named.enumerable).toBe(false);
  expect(named.writable && named.configurable).toBe(true);

  const dom = new HostDOMException('lost', 'AbortError');
  expect(Error.isError(dom)).toBe(true);
  expect(dom instanceof HostDOMException && dom instanceof DOMException && dom instanceof Error).toBe(true);
  expect(Object.getPrototypeOf(HostDOMException) === DOMException).toBe(true);
  expect(dom.name).toBe('AbortError');
  expect(dom.message).toBe('lost');
  expect(dom.code).toBe(20);
  expect(Object.hasOwn(dom, 'stack')).toBe(false);
  const domName = Object.getOwnPropertyDescriptor(DOMException.prototype, 'name').get;
  expect(thrownBy(() => domName.call(Object.create(HostDOMException.prototype))) instanceof TypeError).toBe(true);
  const defaultDom = new HostDOMException();
  expect(defaultDom.name + ':' + defaultDom.message).toBe('Error:');
  let order = '';
  new HostDOMException({ toString() { order += 'm'; return 'lost'; } },
                       { toString() { order += 'n'; return 'AbortError'; } });
  expect(order).toBe('mn');

  expect(thrownBy(() => new ParseError({ toString() { throw token; } })) === token).toBe(true);
  expect(thrownBy(() => new ParseError('bad', { get cause() { throw token; } })) === token).toBe(true);
  const afterGc = new ParseError('still live', { get cause() { gc(); return token; } });
  check(afterGc, 'ParseError', 'still live', token);

  return true;
}
