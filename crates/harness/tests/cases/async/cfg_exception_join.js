// @args [true]
// @pending ["wait:a"]
// @restore
// @reject ["wait:a","bad"]
// @fulfilled 7

import { wait } from "test";

export async function run(flag) {
  let x = 1;
  if (flag) x = 7;
  try {
    x = await wait("a");
  } catch (e) {
    return x;
  }
  return x;
}
