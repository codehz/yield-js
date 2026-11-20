// @pending ["wait:a"]
// @restore
// @reject ["wait:a", "boom"]
// @fulfilled "boom"

import { wait } from 'test';

export async function run() {
  let i = 0;
  try {
    while (i++ < 1) {
      try {
        continue;
      } finally {
        await wait("a");
      }
    }
  } catch (e) {
    return e;
  }
  return "miss";
}
