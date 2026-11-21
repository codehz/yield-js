// @advance 25
// @fulfilled true

import { sleep } from 'test';

export async function run() {
  const saved = new Date();
  await sleep(25);
  return saved.getTime() === 0 && Date.now() === 25 &&
    Date({ valueOf() { throw 'not converted'; } }) === new Date(25).toString();
}
