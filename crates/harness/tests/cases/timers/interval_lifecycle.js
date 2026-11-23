// @advance 1
// @advance 1
// @advance 8
// @fulfilled "1@1,error@1,M@1,2@2"

import { assert } from 'test';

export function run() {
  const order = [];
  const arg = {};
  globalThis.addEventListener('error', event => {
    if (event.error === 'interval first tick') {
      order.push('error@' + Date.now());
      event.preventDefault();
    }
  });
  let ticks = 0;
  const id = setInterval(function (value) {
    assert(this === globalThis && value === arg, 'interval callback receiver and argument');
    ticks++;
    order.push(ticks + '@' + Date.now());
    if (ticks === 1) {
      queueMicrotask(() => order.push('M@' + Date.now()));
      throw 'interval first tick';
    }
    if (ticks === 2) clearTimeout(id);
  }, 1, arg);
  return new Promise(resolve => {
    setTimeout(() => resolve(order.join(',')), 10);
  });
}
