// @pending ["wait:iterator-concat"]
// @restore
// @fulfill ["wait:iterator-concat",0]
// @fulfilled undefined

import { expect, gc, wait } from 'test';

function makeConcat(log, label) {
  return Iterator.concat({
    [Symbol.iterator]() {
      log.push(label + ':open');
      const source = {
        next: (() => {
          let index = 0;
          return function () {
            gc();
            log.push(label + ':next' + index);
            return index < 2 ? { value: ++index, done: false } : { done: true };
          };
        })()
      };
      const wrapped = Iterator.from(source);
      source.next = () => { throw new Error('wrapper lost saved next'); };
      source.return = function () {
        expect(this).toBe(source);
        gc();
        log.push(label + ':close');
        return {};
      };
      return wrapped;
    }
  }, {
    get [Symbol.iterator]() {
      log.push(label + ':get tail');
      return function () {
        gc();
        expect(this.tail).toBe(9);
        log.push(label + ':open tail');
        return [this.tail].values();
      };
    },
    tail: 9
  });
}

export async function run() {
  const log = [];
  const drain = makeConcat(log, 'drain');
  const close = makeConcat(log, 'close');
  expect(drain.next().value).toBe(1);
  expect(close.next().value).toBe(1);
  const before = 'drain:get tail,close:get tail,drain:open,drain:next0,close:open,close:next0';
  expect(log.join()).toBe(before);
  gc();
  await wait('iterator-concat');
  gc();
  expect(drain instanceof Iterator).toBe(true);
  expect(drain.toArray().join()).toBe('2,9');
  expect(drain.next().done).toBe(true);
  const closed = close.return();
  expect(closed.done).toBe(true);
  expect(closed.value).toBe(undefined);
  expect(close.next().done).toBe(true);
  expect(log.join()).toBe(before + ',drain:next1,drain:next2,drain:open tail,close:close');
}
