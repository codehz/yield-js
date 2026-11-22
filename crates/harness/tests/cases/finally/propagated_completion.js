// @pending ["wait:fold"]
// @restore
// @reject ["wait:fold", "rejected"]
// @fulfilled "5:7:rejected|333"

import { wait } from "test";

export async function run() {
  let trace = "";
  async function route(mode) {
    const rhs = 1 + 2;
    try {
      if (mode === 0) return rhs + 2;
      if (mode === 1) throw 7;
      await wait("fold");
    } finally {
      const resumed = await rhs;
      trace += resumed === rhs ? "3" : "?";
    }
  }
  const first = await route(0);
  let caught;
  let rejected;
  try { await route(1); } catch (error) { caught = error; }
  try { await route(2); } catch (error) { rejected = error; }
  return `${first}:${caught}:${rejected}|${trace}`;
}
