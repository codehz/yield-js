// @returns undefined

import { expect } from 'test';

function caught(thunk) {
  try { thunk(); } catch (error) { return error instanceof Error ? error.message : error; }
}

export function run() {
  const inventory = { apple: 1 };
  let calls = 0;
  const buy = function() {
    expect(this).toBe(undefined);
    expect(arguments.length).toBe(0);
    calls += 1;
    if (inventory.apple === 0) throw new Error('apple is out of stock');
    inventory.apple -= 1;
  };
  buy();
  expect(expect(buy).toThrow('out of stock')).toBe(undefined);
  expect(calls).toBe(2);
  expect(buy).toThrow(/OUT OF STOCK/i);
  expect(calls).toBe(3);
  expect(() => { throw 'just a string'; }).toThrow('string');
  expect(() => { throw undefined; }).toThrow();
  expect(() => { throw undefined; }).toThrow(undefined);
  expect(() => { throw { message: 'plain object' }; }).toThrow('plain');
  expect(() => { throw 'x\uD800y'; }).toThrow('\uD800');
  expect(() => { throw '😀'; }).toThrow(/^.$/u);
  expect(() => { throw '😀'; }).toThrow(/^..$/);
  expect(() => { throw 'nonempty'; }).toThrow('');
  expect(() => { throw new Error(); }).toThrow(/^$/);

  const regexp = /stock/g;
  regexp.lastIndex = 19;
  regexp.exec = regexp.test = () => { throw 'must not call regexp methods'; };
  expect(buy).toThrow(regexp);
  expect(regexp.lastIndex).toBe(19);
  const sticky = /stock/y;
  sticky.lastIndex = 1;
  expect(() => { throw 'stockpile'; }).toThrow(sticky);
  expect(caught(() => expect(() => { throw ' stock'; }).toThrow(sticky)))
    .toBe('toThrow: expected message matching /stock/y, actual " stock"');
  expect(sticky.lastIndex).toBe(1);

  const before = calls;
  expect(caught(() => expect(buy).toThrow(42))).toBe('toThrow: expected a string or RegExp');
  expect(caught(() => expect(buy).toThrow(Object.create(RegExp.prototype))))
    .toBe('toThrow: expected a string or RegExp');
  expect(calls).toBe(before);
  expect(caught(() => expect(inventory).toThrow())).toBe('toThrow: expected a callable');
  expect(caught(() => expect(buy).toThrow(/plums/i)))
    .toBe('toThrow: expected message matching /plums/i, actual "apple is out of stock"');
  expect(calls).toBe(before + 1);
  expect(caught(() => expect(() => { calls += 1; }).toThrow()))
    .toBe('toThrow: function returned without throwing');
  expect(calls).toBe(before + 2);

  let getterCalls = 0;
  const withGetter = { get message() { getterCalls += 1; return 'hidden'; } };
  expect(caught(() => expect(() => { throw withGetter; }).toThrow('hidden')))
    .toBe('toThrow: thrown value has no string message');
  expect(getterCalls).toBe(0);
  expect(caught(() => expect(() => { throw Object.create(Error.prototype); }).toThrow('')))
    .toBe('toThrow: thrown value has no string message');

  expect(caught(() => expect(async () => { calls += 1; throw new Error('rejected'); }).toThrow()))
    .toBe('toThrow: function returned without throwing');
  expect(calls).toBe(before + 3);

  expect(() => {
    expect(2 + 2).toBe(4);
    expect(() => { throw 'inner'; }).toThrow(/inner/);
    throw new Error('outer');
  }).toThrow('outer');
}
