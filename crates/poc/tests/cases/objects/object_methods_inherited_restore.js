// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a",0]
// @fulfilled true

import { wait } from 'test';

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
  return o.n === 7 && C.prototype.n === undefined && f === o.m &&
    ((o.m))() === 9 && (await o.a()) === 9 && o.m.prototype === undefined;
}
