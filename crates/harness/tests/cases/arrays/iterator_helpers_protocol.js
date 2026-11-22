// @returns undefined

import { expect } from 'test';

export function run() {
  const proto = Object.getPrototypeOf(Object.getPrototypeOf([].values()));
  const tag = Symbol.toStringTag;
  const derived = Object.create(proto);
  expect(proto[tag]).toBe('Iterator');
  expect(derived[tag]).toBe('Iterator');
  derived[tag] = 'custom';
  expect(Object.getOwnPropertyDescriptor(derived, tag).enumerable).toBe(true);
  expect(derived[tag]).toBe('custom');
  let log = '';
  const source = {
    get next() {
      log += 'get;';
      let i = 0;
      return function () {
        const n = i++;
        log += 'next' + n + ';';
        return { done: false, get value() { log += 'value' + n + ';'; return n; } };
      };
    },
    return() { log += 'return;'; return {}; }
  };
  const helper = proto.drop.call(source, 2).take(1);
  expect(log).toBe('get;');
  expect(helper.next().value).toBe(2);
  expect(log).toBe('get;next0;next1;next2;value2;');
  expect(helper.next().done).toBe(true);
  expect(helper.return(12).value).toBe(undefined);
  expect(log).toBe('get;next0;next1;next2;value2;return;');

  log = '';
  let error;
  try { proto.take.call(source, NaN); } catch (e) { error = e; }
  expect(error instanceof RangeError).toBe(true);
  expect(log).toBe('return;');
  expect(proto.take.call(source, -0.5).next().done).toBe(true);
  expect(log).toBe('return;get;return;');

  let inner;
  log = '';
  const flat = proto.flatMap.call(source, () => {
    inner = {
      next() { return {value: 7, done: false}; },
      return() { log += 'inner;'; throw 'first'; }
    };
    return inner;
  });
  expect(flat.next().value).toBe(7);
  try { flat.return(); } catch (e) { error = e; }
  expect(error).toBe('first');
  expect(log).toBe('get;next0;value0;inner;return;');
  expect(flat.next().done).toBe(true);

  const uncaught = { next() { throw 'next'; }, return() { log += 'unexpected;'; } };
  log = '';
  try { proto.map.call(uncaught, x => x).next(); } catch (e) { error = e; }
  expect(error).toBe('next');
  expect(log).toBe('');
  try { proto.every.call({ next() { return {value: 1}; }, return() { throw 'close'; } }, () => { throw 'callback'; }); }
  catch (e) { error = e; }
  expect(error).toBe('callback');

  const closure = { next() { return {value: 1}; }, return() { throw 'close'; } };
  try { proto.some.call(closure, () => true); } catch (e) { error = e; }
  expect(error).toBe('close');

  let active;
  active = [3].values().map(value => {
    let reentrant;
    try { active.next(); } catch (e) { reentrant = e; }
    expect(reentrant instanceof TypeError).toBe(true);
    return value;
  });
  expect(active.next().value).toBe(3);
  expect(active.next().done).toBe(true);
  let unopened;
  unopened = proto.map.call({ return() { expect(unopened.next().done).toBe(true); return {}; } }, x => x);
  expect(unopened.return().done).toBe(true);
}
