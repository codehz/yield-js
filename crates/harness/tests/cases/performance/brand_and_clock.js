// @advance 2
// @advance 5
// @fulfilled "0,2,7"

import { assert } from 'test';

function expectTypeError(call, label) {
  try { call(); } catch (error) {
    assert(error instanceof TypeError, label);
    return;
  }
  assert(false, label);
}

export function run() {
  expectTypeError(() => Performance(), 'illegal interface call');
  expectTypeError(() => new Performance(), 'illegal interface construction');
  const p = globalThis.performance;
  const proto = Performance.prototype;
  const origin = Object.getOwnPropertyDescriptor(proto, 'timeOrigin').get;
  const fake = Object.create(proto);
  for (const fn of [proto.now, proto.toJSON, origin]) {
    expectTypeError(() => fn(), 'detached native rejects missing receiver');
    expectTypeError(() => fn.call(fake), 'prototype forgery does not confer brand');
    expectTypeError(() => fn.call(new EventTarget()), 'EventTarget is not Performance');
  }
  expectTypeError(() => { p.timeOrigin = 1; }, 'timeOrigin has no setter');

  const first = p.toJSON();
  const second = p.toJSON();
  assert(first !== second && Object.getPrototypeOf(first) === Object.prototype, 'toJSON returns a fresh ordinary object');
  assert(Object.keys(first).join(',') === 'timeOrigin' && first.timeOrigin === 0 && second.timeOrigin === 0,
    'toJSON serializes only timeOrigin');
  first.timeOrigin = 123;
  assert(second.timeOrigin === 0 && p.timeOrigin === 0, 'JSON values are independent of the singleton');

  const event = new Event('sample');
  let observed;
  p.addEventListener('sample', function (value) {
    observed = this === p && value === event && value.target === p && value.currentTarget === p;
  });
  assert(p.dispatchEvent(event) && observed, 'Performance supports EventTarget listeners');

  const times = [p.now()];
  return new Promise(resolve => {
    setTimeout(() => times.push(p.now()), 2);
    setTimeout(() => {
      times.push(p.now());
      assert(p.timeOrigin === 0 && p.toJSON().timeOrigin === 0, 'timeOrigin stays fixed as the clock advances');
      resolve(times.join(','));
    }, 7);
  });
}
