// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a",0]
// @fulfilled 23

import { wait } from 'test';

export async function run() {
  const it = [1, 2].values();
  if (it.next().value !== 1) {
    throw "iterator did not start at the first element";
  }
  await wait("a");
  return it.next().value * 10 + [3].values().next().value;
}
