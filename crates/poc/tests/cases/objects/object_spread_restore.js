// @pending ["wait:source"]
// @restore
// @fulfill ["wait:source", 7]
// @fulfilled "before,after|6,7,8"

import { wait } from 'test';

export async function run() {
  let trace = "";
  const result = {
    ...{ get first() { trace += "before,"; return 6; } },
    ...{ middle: await wait("source") },
    ...{ get last() { trace += "after"; return 8; } },
  };
  return trace + "|" + result.first + "," + result.middle + "," + result.last;
}
