// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", null]
// @fulfilled true

import { wait } from 'test';

export async function run() {
  const before = globalThis;
  globalThis.marker = 7;
  await wait("a");
  return globalThis.marker === 7 && globalThis === globalThis &&
    globalThis === before && globalThis.globalThis === globalThis &&
    globalThis.Promise === Promise;
}
