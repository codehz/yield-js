// @returns "ok"

import { spawnDelayedTask } from "test";

function schedule() {
  spawnDelayedTask(null, 0);
}

export function run() {
  try {
    schedule();
    return "missing TypeError";
  } catch (error) {
    if (!(error instanceof TypeError)) return "wrong error type";
    if (error.message !== "expected a callable") return error.message;
    const expected = "TypeError: expected a callable\n    at schedule (main.js:6:3)\n    at run (main.js:11:5)";
    return error.stack === expected ? "ok" : error.stack;
  }
}
