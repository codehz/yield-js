// @pending ["wait:release"]
// @restore
// @fulfill ["wait:release", null]
// @fulfilled true

import { wait } from "test";

export async function run() {
  let trace = "";
  function* sync() {
    using resource = { [Symbol.dispose]() { trace += "S"; } };
    yield 1;
  }
  const s = sync();
  if (s.next().value !== 1 || trace !== "") return false;
  if (s.return(2).value !== 2 || trace !== "S" || !s.next().done) return false;
  const t = sync();
  t.next();
  let caught = false;
  try { t.throw(undefined); } catch (e) { caught = e === undefined; }
  if (!caught || trace !== "SS" || !t.next().done) return false;

  async function* asyncGen() {
    await using resource = { [Symbol.asyncDispose]() { trace += "A"; return wait("release"); } };
    yield 3;
  }
  const g = asyncGen();
  if ((await g.next()).value !== 3 || trace !== "SS") return false;
  const first = g.return(4).then(result => { trace += "1"; return result; });
  const second = g.next().then(result => { trace += "2"; return result; });
  const a = await first;
  const b = await second;
  return trace === "SSA12" && a.done && a.value === 4 && b.done && b.value === undefined;
}
