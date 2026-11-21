// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a",0]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  function C() {}
  const b = 2;
  C.prototype = {
    get x() { return this.n + b; },
    set x(v) { this.n = v - b; },
    m() { return this.x; },
    async a() { return this.x; }
  };
  const o = new C();
  o.x = 9;
  const f = o.m;
  await wait("a");
  expect(o.n).toBe(7);
  expect(C.prototype.n).toBe(undefined);
  expect(f).toBe(o.m);
  expect(((o.m))()).toBe(9);
  expect((await o.a())).toBe(9);
  expect(o.m.prototype).toBe(undefined);
}
