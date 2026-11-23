// @returns true

import { assert } from 'test';

function expectTypeError(call, label) {
  try { call(); } catch (error) {
    assert(error instanceof TypeError, label);
    return;
  }
  assert(false, label);
}

export function run() {
  for (const [name, fn, length] of [
    ['setTimeout', setTimeout, 1], ['setInterval', setInterval, 1],
    ['clearTimeout', clearTimeout, 0], ['clearInterval', clearInterval, 0],
  ]) {
    const d = Object.getOwnPropertyDescriptor(globalThis, name);
    assert(d.value === fn && d.writable && d.enumerable && d.configurable, name);
    const n = Object.getOwnPropertyDescriptor(fn, 'name');
    const l = Object.getOwnPropertyDescriptor(fn, 'length');
    assert(n.value === name && !n.writable && !n.enumerable && n.configurable, name + '.name');
    assert(l.value === length && !l.writable && !l.enumerable && l.configurable, name + '.length');
    expectTypeError(() => new fn(() => {}), name + ' is not a constructor');
  }

  for (const set of [setTimeout, setInterval]) {
    expectTypeError(() => set(), 'missing callback');
    let converted = false;
    const delay = { valueOf() { converted = true; return 0; } };
    expectTypeError(() => set('throw 1', delay), 'string callback');
    assert(!converted, 'callback validation precedes delay conversion');
    expectTypeError(() => set({}, delay), 'object callback');
    assert(!converted, 'non-callable object does not convert delay');
  }

  for (const clear of [clearTimeout, clearInterval]) {
    assert(clear() === undefined && clear(-1) === undefined, 'clear is a no-op for unknown IDs');
  }
  return true;
}
