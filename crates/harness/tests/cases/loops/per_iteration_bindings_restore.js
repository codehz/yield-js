// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 1]
// @fulfilled 1

import { wait } from "test";

export async function run() {
  let read;
  for (let value of [1, 2]) {
    if (value === 1) {
      read = () => value;
      await wait("a");
    } else {
      return read();
    }
  }
  return "exhausted";
}
