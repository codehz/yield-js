// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a",0]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  let reads = 0;
  const source = { get length() { reads++; return this.size; }, size:1, 0:'a' };
  const keys = Array.prototype.keys.call(source);
  const entries = Array.prototype.entries.call(source);
  expect(reads).toBe(0);
  expect(keys.next().value).toBe(0);
  const first = entries.next().value;
  expect(first.join()).toBe('0,a');
  await wait('a');
  source[1] = 'b'; source.size = 2;
  expect(keys.next().value).toBe(1);
  const second = entries.next().value;
  expect(second.join()).toBe('1,b');
  expect(first.join()).toBe('0,a');
  expect(keys.next().done).toBe(true);
  expect(entries.next().done).toBe(true);
  source.size = 3;
  expect(keys.next().done).toBe(true);
  expect(entries.next().done).toBe(true);
  expect(reads).toBe(6);

  const throws = {length:2, get 0() { throw 'index'; }, 1:'next'};
  expect(Array.prototype.keys.call(throws).next().value).toBe(0);
  const it = Array.prototype.entries.call(throws);
  let error;
  try { it.next(); } catch (e) { error = e; }
  expect(error).toBe('index');
  expect(it.next().value.join()).toBe('1,next');
  expect(Array.prototype.values.call('xy').next().value).toBe('x');
}
