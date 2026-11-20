// @returns undefined

import { expect } from 'test';

export function run() {
  function receiver() { return this; }
  let o = {};
  o.f = receiver;
  let detached = o.f;
  expect(((o.f))()).toBe(o);
  expect((o["f"])()).toBe(o);
  expect((detached)()).toBe(undefined);
}
