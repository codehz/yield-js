// @returns undefined

import { expect } from 'test';

function check(a, b, ...extra) {
  return this.base === 10 && a === 2 && b === undefined && extra.length === 1 && extra[0] === 4;
}

export function run() {
  const apply = Reflect.apply;
  const receiver = {base: 10};
  expect(apply(check, receiver, [2, , 4])).toBe(true);
  let trace = '';
  const list = Object.create({get 0() { trace += '0'; return 2; }});
  Object.defineProperty(list, 'length', {get() {
    trace += 'l';
    return {valueOf() { trace += 'v'; return '3.8'; }};
  }});
  Object.defineProperty(list, '2', {get() { trace += '2'; return 4; }});
  expect(apply(check, receiver, list)).toBe(true);
  expect(trace).toBe('lv02');
  function missing(a, b) { return a === undefined && b === undefined && this === undefined; }
  expect(apply(missing, undefined, {})).toBe(true);
  expect(apply(missing, undefined, {length: -1, 0: 1})).toBe(true);
  let invalidTarget;
  try { apply({}, null, []); } catch (e) { invalidTarget = e; }
  expect(invalidTarget instanceof TypeError).toBe(true);
  let invalidList;
  try { apply(check, receiver, 'abc'); } catch (e) { invalidList = e; }
  expect(invalidList instanceof TypeError).toBe(true);
  let missingList;
  try { apply(check, receiver); } catch (e) { missingList = e; }
  expect(missingList instanceof TypeError).toBe(true);
  const thrown = {};
  let targetError;
  try { apply(() => { throw thrown; }, null, []); } catch (e) { targetError = e; }
  expect(targetError).toBe(thrown);
  let getterError;
  try { apply(check, receiver, {length: 1, get 0() { throw thrown; }}); }
  catch (e) { getterError = e; }
  expect(getterError).toBe(thrown);
}
