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
}
