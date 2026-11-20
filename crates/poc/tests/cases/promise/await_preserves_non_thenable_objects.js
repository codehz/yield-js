// @pending ["wait:objects"]
// @restore
// @fulfill ["wait:objects", 0]
// @fulfilled undefined

import { expect, bind, wait } from 'test';

export async function run() {
  const obj = {value: 7};
  const array = [3];
  const fn = () => 5;
  const native = bind("await-node");
  let reads = 0;
  const noThen = {get then() { reads += 1; return false; }};
  let tick = false;
  queueMicrotask(() => { tick = true; });
  const result = await obj;
  expect(tick).toBe(true);
  expect(result).toBe(obj);
  expect(await array).toBe(array);
  expect(await fn).toBe(fn);
  expect(await native).toBe(native);
  expect(await noThen).toBe(noThen);
  expect(reads).toBe(1);
  await wait("objects");
  expect(result).toBe(obj);
  expect(result.value).toBe(7);
}
