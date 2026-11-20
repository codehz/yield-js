// @pending ["wait:collections"]
// @restore
// @fulfill ["wait:collections", null]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  const key = {};
  const symbol = Symbol('key');
  const m = new Map([[key, symbol], ['gone', 0], [symbol, key], ['last', { marker: 7 }]]);
  const i = m.entries();
  expect(i.next().value[0]).toBe(key);
  m.delete('gone');
  const j = new Set([key, symbol, 'last']).values();
  expect(j.next().value).toBe(key);
  await wait('collections');
  expect(m.get(key)).toBe(symbol);
  expect(m.get(symbol)).toBe(key);
  expect(m.size).toBe(3);
  m.set('append', 9);
  const middle = i.next().value;
  const last = i.next().value;
  expect(middle[0]).toBe(symbol);
  expect(middle[1]).toBe(key);
  expect(last[0]).toBe('last');
  expect(last[1].marker).toBe(7);
  expect(i.next().value[1]).toBe(9);
  expect(i.next().done).toBe(true);
  expect(j.next().value).toBe(symbol);
  expect(j.next().value).toBe('last');
  expect(j.next().done).toBe(true);
}
