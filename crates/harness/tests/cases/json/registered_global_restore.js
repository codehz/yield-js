// @pending ["wait:json"]
// @restore
// @fulfill ["wait:json", 0]
// @fulfilled true

import { wait } from 'test';

export async function run() {
  const parse = globalThis.JSON.parse;
  const makeRaw = JSON.rawJSON;
  const isRaw = JSON.isRawJSON;
  const raw = makeRaw('9007199254740993');
  await wait("json");
  return JSON === globalThis.JSON && parse === JSON.parse && JSON.stringify(parse('[2]')) === '[2]'
    && makeRaw === JSON.rawJSON && isRaw === JSON.isRawJSON && isRaw(raw)
    && Object.isFrozen(raw) && JSON.stringify(raw) === '9007199254740993';
}
