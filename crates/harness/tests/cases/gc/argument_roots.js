// @returns undefined

import { expect, gc } from 'test';

// Makes one heap object unreachable so the following collection really sweeps.
function collect() {
  let garbage = { token: 'garbage' };
  garbage = null;
  return gc();
}

function identity(first, second) {
  expect(collect() > 0).toBe(true);
  return first === second;
}

function collect_arguments() {
  expect(collect() > 0).toBe(true);
  return arguments[0] === arguments[1];
}

function collect_rest(...values) {
  expect(collect() > 0).toBe(true);
  return values[0] === values[1];
}

export function run() {
  const shared = { token: 'shared' };
  expect(identity(shared, shared)).toBe(true);
  expect(identity.bind(null, shared)(shared)).toBe(true);
  expect(identity(...[shared, shared])).toBe(true);
  expect(identity.apply(null, [shared, shared])).toBe(true);
  expect(collect_arguments(shared, shared)).toBe(true);
  expect(collect_rest(shared, shared)).toBe(true);

  // The field initializer sweeps before the constructor's parameters are
  // bound, while the arguments still live only in the caller's frame slots.
  class Collecting {
    field = (expect(collect() > 0).toBe(true), 1);
    constructor(value) { this.value = value; }
  }
  const fresh = { token: 'fresh' };
  const constructed = new Collecting(fresh);
  expect(constructed.field).toBe(1);
  expect(constructed.value).toBe(fresh);
  expect(constructed.value.token).toBe('fresh');
  const spread = new Collecting(...[fresh]);
  expect(spread.value).toBe(fresh);

  class Base {
    field = (expect(collect() > 0).toBe(true), 2);
    constructor(value) { this.value = value; }
  }
  class Derived extends Base {
    constructor(value) { super(value); }
  }
  const derived = new Derived(fresh);
  expect(derived.field).toBe(2);
  expect(derived.value).toBe(fresh);
  const derivedSpread = new Derived(...[fresh]);
  expect(derivedSpread.value).toBe(fresh);
}
