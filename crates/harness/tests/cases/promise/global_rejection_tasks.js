// @advance 1
// @fulfilled true

import { gc, sleep } from 'test';

export async function run() {
  const log = [];
  let a, b, late;
  const reason = {};
  onunhandledrejection = e => {
    gc();
    if (!(e instanceof PromiseRejectionEvent) || !e.isTrusted || !e.cancelable || e.bubbles) log.push('bad event');
    if (e.promise === a && e.reason === reason) {
      log.push('A');
      e.promise.catch(() => {});
      b.catch(() => {});
      queueMicrotask(() => log.push('microtask'));
      late = Promise.reject('C');
    } else if (e.promise === late && e.reason === 'C') {
      log.push('C');
    } else {
      log.push('unexpected');
    }
    return false;
  };
  onrejectionhandled = e => {
    e.preventDefault();
    log.push(e.promise === late && e.reason === 'C' && e.isTrusted && !e.cancelable &&
        !e.defaultPrevented ? 'handled C' : 'bad handled');
  };
  a = Promise.reject(reason);
  b = Promise.reject('B');
  const early = Promise.reject('early');
  queueMicrotask(() => early.catch(() => {}));
  await sleep(1);
  late.catch(() => {});
  late.catch(() => {});
  await new Promise(resolve => {
    globalThis.addEventListener('rejectionhandled', () => resolve(), { once: true });
  });
  return log.join(',') === 'A,microtask,C,handled C';
}
