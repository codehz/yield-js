// @advance 1
// @fulfilled "A,M,B"

import { assert } from 'test';

export function run() {
  const order = [];
  const receiver = {};
  const arg = {};
  const key = Symbol();

  return new Promise(resolve => {
    let canceled;
    const a = setTimeout.call(receiver, function (first, second, third) {
      assert(this === globalThis, 'timer callback receiver');
      assert(first === arg && second === key && third === receiver, 'timer callback arguments');
      order.push('A');
      assert(clearInterval(canceled) === undefined, 'clearInterval cancels a queued timeout');
      queueMicrotask(() => order.push('M'));
    }, 1, arg, key, receiver);
    canceled = setTimeout(() => order.push('canceled timeout'), 1);
    const interval = setInterval(() => order.push('canceled interval'), 1);
    assert(clearTimeout(interval) === undefined, 'clearTimeout cancels an interval');
    const b = setTimeout(() => {
      order.push('B');
      resolve(order.join(','));
    }, 1);
    assert(Number.isInteger(a) && a > 0 && a <= 2147483647, 'positive long ID');
    assert(a !== canceled && a !== interval && a !== b && canceled !== interval && canceled !== b,
      'active timeouts and intervals share the ID pool');
  });
}
