// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a",0]
// @fulfilled true

import { seed, wait } from 'test';

export async function run() {
  const v = seed();
  const object = {get x() { throw 3; }};
  await wait("a");
  try {
    return object.x;
  } catch (e) {
    return v + e === 13;
  }
}
