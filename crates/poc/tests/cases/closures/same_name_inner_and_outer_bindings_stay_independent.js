// @restore
// @fulfill ["wait:a",0]
// @fulfilled 1

import { wait } from 'test';

export async function run() {
  const x = {n: 1};
  const inner = () => x;
  await wait("a");
  {
    const x = {n: 2};
    return inner().n;
  }
}
