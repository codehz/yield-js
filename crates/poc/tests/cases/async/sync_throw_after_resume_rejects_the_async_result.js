// @fulfill ["wait:a",1]
// @rejected "sync boom"

import { wait } from 'test';

function fail() {
  throw "sync boom";
}
export async function run() {
  await wait("a");
  fail();
  return 1;
}
