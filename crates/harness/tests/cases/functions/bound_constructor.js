// @returns undefined

import { expect } from 'test';

function Target(a, b, c) { this.args = [a, b, c].join(','); }
function Alternate() { throw 'newTarget body must not run'; }

export function run() {
  const receiver = {};
  const bound = Target.bind(receiver, 1, 2);
  expect(bound.name).toBe('bound Target');
  expect(bound.length).toBe(1);
  expect(Target.bind(null, 1, 2, 3, 4).length === 0).toBe(true);
  expect('prototype' in bound).toBe(false);

  const instance = new bound(3);
  const reflected = Reflect.construct(bound, [4]);
  const explicit = Reflect.construct(bound, [5], bound);
  const alternate = Reflect.construct(bound, [6], Alternate);
  expect(instance.args).toBe('1,2,3');
  expect(instance instanceof Target).toBe(true);
  expect(instance instanceof bound).toBe(true);
  expect(Object.getPrototypeOf(instance)).toBe(Target.prototype);
  expect(reflected.args).toBe('1,2,4');
  expect(Object.getPrototypeOf(reflected)).toBe(Target.prototype);
  expect(explicit.args).toBe('1,2,5');
  expect(Object.getPrototypeOf(explicit)).toBe(Target.prototype);
  expect(alternate.args).toBe('1,2,6');
  expect(Object.getPrototypeOf(alternate)).toBe(Alternate.prototype);
  expect(receiver.args).toBe(undefined);
  const reads = [];
  const parent = Object.create(Function.prototype);
  Object.setPrototypeOf(Target, parent);
  Object.defineProperty(Target, 'length', {get() { reads.push('length'); return 4.9; }});
  Object.defineProperty(Target, 'name', {get() { reads.push('name'); return 'custom'; }});
  const first = Target.bind(null, 8);
  expect(reads.join(',')).toBe('length,name');
  expect(first.length).toBe(3);
  expect(first.name).toBe('bound custom');
  expect(Object.getPrototypeOf(first)).toBe(parent);
  Object.setPrototypeOf(Target, Function.prototype);
  Object.defineProperty(Target, 'name', {value: 'changed'});
  Object.defineProperty(Target, 'length', {value: Infinity});
  expect(Target.bind(null, 1).length).toBe(Infinity);
  const second = first.bind(null, 9);
  expect(second.name).toBe('bound bound custom');
  expect(second.length).toBe(2);
  Object.defineProperty(first, 'name', {value: 'rewritten'});
  Object.defineProperty(first, 'length', {value: 12});
  expect(second.name).toBe('bound bound custom');
  expect(second.length).toBe(2);
  expect(first.toString()).toBe('function bound custom() { [native code] }');
  const layered = new second(10);
  expect(layered.args).toBe('8,9,10');
  expect(layered instanceof second && layered instanceof first && layered instanceof Target).toBe(true);
  Object.defineProperty(parent, 'length', {get() { throw 'inherited length read'; }});
  delete first.length;
  expect(first.bind(null).length).toBe(0);
  Object.defineProperty(Target, 'length', {get() { throw 41; }});
  let caught;
  try { Target.bind(null); } catch (e) { caught = e; }
  expect(caught).toBe(41);
}
