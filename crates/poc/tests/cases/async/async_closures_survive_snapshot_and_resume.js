// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a",4]
// @fulfilled 5

import { wait } from 'test';

export async function run() {
  const f = async () => {
    const x = await wait("a");
    return x + 1;
  };
  return await f();
}
