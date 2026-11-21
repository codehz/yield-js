// @pending ["wait:b"]
// @restore
// @fulfill ["wait:b", 0]
// @fulfilled "1:2:3:true"

import { wait } from 'test';

export async function run() {
  async function* g() {
    yield 1;
    yield 2;
    return 3;
  }
  const it = g();
  const first = await it.next();
  // The generator is suspended at the first yield with an empty request queue.
  await wait("b");
  const second = await it.next();
  const third = await it.next();
  return `${first.value}:${second.value}:${third.value}:${third.done}`;
}
