// @returns true

import { assert } from 'test';

export function run() {
  assert(self === globalThis, 'self aliases globalThis');
  const selfGlobal = Object.getOwnPropertyDescriptor(globalThis, 'self');
  assert(selfGlobal.value === globalThis && selfGlobal.writable && selfGlobal.enumerable && selfGlobal.configurable,
    'self global descriptor');

  const navigatorGlobal = Object.getOwnPropertyDescriptor(globalThis, 'navigator');
  assert(navigatorGlobal.value === navigator && navigatorGlobal.writable && navigatorGlobal.enumerable && navigatorGlobal.configurable,
    'navigator global descriptor');
  assert(Object.getPrototypeOf(navigator) === Object.prototype, 'navigator is an ordinary object');
  assert(Object.keys(navigator).join(',') === 'userAgent,platform', 'navigator exposes exactly two attributes');

  assert(navigator.userAgent === 'yield-js', 'host default user agent');
  assert(typeof navigator.platform === 'string' && navigator.platform.includes('-'), 'compile-target platform');

  for (const key of ['userAgent', 'platform']) {
    const descriptor = Object.getOwnPropertyDescriptor(navigator, key);
    assert(descriptor && !descriptor.writable && descriptor.enumerable && descriptor.configurable,
      key + ' is a readonly configurable data attribute');
  }
  let threw = false;
  try {
    navigator.userAgent = 'other';
  } catch (error) {
    threw = error instanceof TypeError;
  }
  assert(threw && navigator.userAgent === 'yield-js', 'user agent is not writable');
  return true;
}
