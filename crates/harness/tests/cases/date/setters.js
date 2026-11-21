// @returns undefined

import { expect, gc } from 'test';

export function run() {
  const d = new Date(0);
  expect(d.setUTCFullYear(99)).toBe(Date.parse('0099'));
  expect(d.setYear(99)).toBe(Date.UTC(1999));
  expect(d.getYear()).toBe(99);
  expect(d.setUTCMonth(1, 29)).toBe(Date.UTC(1999, 2, 1));
  expect(d.setUTCDate(0)).toBe(Date.UTC(1999, 1, 28));
  let order = '';
  const h = { valueOf() { d.setTime(0); order += 'h'; gc(); return 23; } };
  const m = { valueOf() { order += 'm'; gc(); return 58; } };
  expect(d.setUTCHours(h, m, 57, 555)).toBe(Date.UTC(1999, 1, 28, 23, 58, 57, 555));
  expect(order).toBe('hm');
  expect(d.setUTCMinutes(60)).toBe(Date.UTC(1999, 2, 1, 0, 0, 57, 555));
  expect(d.setUTCSeconds(-1)).toBe(Date.UTC(1999, 1, 28, 23, 59, 59, 555));
  expect(d.setUTCMilliseconds(-1)).toBe(Date.UTC(1999, 1, 28, 23, 59, 58, 999));
  expect(Number.isNaN(d.setHours(1, undefined))).toBe(true);
  const result = d.setUTCMonth({ valueOf() { d.setTime(7); return 2; } });
  expect(Number.isNaN(result)).toBe(true);
  expect(d.getTime()).toBe(7);
  d.setTime(undefined);
  expect(d.setFullYear(2000)).toBe(Date.UTC(2000));
  d.setTime(NaN);
  expect(d.setTime(-1.9)).toBe(-1);
  expect(d.getMilliseconds()).toBe(999);
}
