// @pending ["wait:branch"]
// @restore
// @fulfill ["wait:branch", 0]
// @fulfilled "1:7"

import { wait } from "test";

export async function run() {
  let calls = 0;
  const value = { valueOf() { calls++; throw 7; } };
  try {
    await wait("branch");
    if (value < 10) {} else {}
    return "comparison skipped";
  } catch (error) {
    return calls + ":" + error;
  }
}
