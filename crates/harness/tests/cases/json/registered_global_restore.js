// @pending ["wait:json"]
// @restore
// @fulfill ["wait:json", 0]
// @fulfilled true

import { wait } from 'test';

export async function run() {
  const parse = globalThis.JSON.parse;
  await wait("json");
  return JSON === globalThis.JSON && parse === JSON.parse && JSON.stringify(parse('[2]')) === '[2]';
}
