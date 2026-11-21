// @returns undefined

import { expect } from 'test';

export function run() {
  const before = new Date(-1);
  expect(before.toISOString()).toBe('1969-12-31T23:59:59.999Z');
  expect(before.getUTCDay()).toBe(3);
  expect(before.getUTCMilliseconds()).toBe(999);
  const limit = 8640000000000000;
  for (const time of [-limit, limit]) {
    const d = new Date(time);
    expect(Date.parse(d.toISOString())).toBe(time);
    expect(Date.parse(d.toString())).toBe(time);
    expect(Date.parse(d.toUTCString())).toBe(time);
  }
  expect(new Date(-limit).toISOString()).toBe('-271821-04-20T00:00:00.000Z');
  expect(new Date(limit).toISOString()).toBe('+275760-09-13T00:00:00.000Z');
  expect(Number.isNaN(new Date(limit + 1).getTime())).toBe(true);
  expect(Number.isNaN(new Date(undefined).getTime())).toBe(true);
  expect(Number.isNaN(Date.UTC())).toBe(true);
  expect(Number.isNaN(Date.UTC(2000, undefined))).toBe(true);
  expect(new Date(Date.UTC(0)).getUTCFullYear()).toBe(1900);
  expect(new Date(Date.UTC(1900, 1, 29)).toISOString()).toBe('1900-03-01T00:00:00.000Z');
  expect(new Date(Date.UTC(2000, 1, 29)).toISOString()).toBe('2000-02-29T00:00:00.000Z');
  expect(new Date(Date.UTC(2000, 12, 0, 24)).toISOString()).toBe('2001-01-01T00:00:00.000Z');
  expect(new Date(2024.9, -1.8).toISOString()).toBe('2023-12-01T00:00:00.000Z');
  expect(Date.parse('1970')).toBe(0);
  expect(Date.parse('1970-01T00:00')).toBe(0);
  expect(Date.parse('1969-12-31T24:00:00.000Z')).toBe(0);
  expect(Date.parse('1970-01-01T05:45:00+05:45')).toBe(0);
  for (const invalid of ['-000000-01-01', '1970-01-01T24:00:01Z', '1970-00-01',
      '1970-01-01T00:00+00:60', '01/01/1970', '1969-12-31T23:59:60Z']) {
    expect(Number.isNaN(Date.parse(invalid))).toBe(true);
  }
}
