// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 1]
// @fulfilled "late"

import { wait } from 'test';

export async function run() {
  return await Promise.resolve("late").finally(function () {
    return wait("a");
  });
}
