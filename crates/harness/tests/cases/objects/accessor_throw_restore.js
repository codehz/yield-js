// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a",0]
// @fulfilled undefined

import { expect, seed, wait } from 'test';

export async function run() {
  const v = seed();
  const object = {get x() { throw 3; }};
  await wait("a");
  let error;
  try {
    object.x;
  } catch (caught) {
    error = caught;
  }
  expect(error).toBe(3);
  expect(v + error).toBe(13);
}
