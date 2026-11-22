// @pending ["wait:return"]
// @restore
// @reject ["wait:return", "cleanup failed"]
// @fulfilled true

import { gc, wait } from "test";

export async function run() {
  let steps = 0;
  let closes = 0;
  const reason = { message: "mapping failed" };
  const input = {
    [Symbol.asyncIterator]() {
      return {
        next() { steps++; return { done: false, value: 4 }; },
        return() { closes++; gc(); return wait("return"); }
      };
    }
  };
  try {
    await Array.fromAsync(input, () => Promise.reject(reason));
  } catch (error) {
    return error === reason && steps === 1 && closes === 1;
  }
  return false;
}
