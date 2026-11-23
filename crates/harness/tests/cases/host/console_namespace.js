// @returns true
import { assert } from 'test';

export function run() {
  const global = Object.getOwnPropertyDescriptor(globalThis, 'console');
  assert(global.value === console && global.writable && global.configurable && !global.enumerable);
  assert(typeof console === 'object' && !('Console' in globalThis));
  const proto = Object.getPrototypeOf(console);
  assert(proto !== Object.prototype && Object.getPrototypeOf(proto) === Object.prototype);
  assert(Reflect.ownKeys(proto).length === 0);
  assert(Object.keys(console).length === 19);
  for (const name of ['assert', 'clear', 'count', 'countReset', 'debug', 'dir', 'dirxml',
    'error', 'group', 'groupCollapsed', 'groupEnd', 'info', 'log', 'table', 'time',
    'timeEnd', 'timeLog', 'trace', 'warn']) {
    const d = Object.getOwnPropertyDescriptor(console, name);
    assert(d.enumerable && d.configurable && d.writable && d.value.length === 0);
    assert(d.value.name === name && !('prototype' in d.value));
    try { new d.value(); } catch (error) { assert(error instanceof TypeError); continue; }
    assert(false);
  }
  const tag = Symbol.toStringTag;
  const descriptor = Object.getOwnPropertyDescriptor(console, tag);
  assert(descriptor.value === 'console' && descriptor.configurable && !descriptor.writable && !descriptor.enumerable);
  assert(console.toString() === '[object console]');
  Object.defineProperty(console, tag, { value: 'Test' });
  assert(Object.prototype.toString.call(console) === '[object Test]');
  assert(delete console[tag]);
  assert(console.toString() === '[object Object]');
  const original = console;
  const log = console.log;
  assert(log('detached') === undefined);
  assert(((console) => console.log())({ log: () => 42 }) === 42);
  console = 7;
  assert(globalThis.console === 7);
  globalThis.console = original;
  assert(console === original);
  return true;
}
