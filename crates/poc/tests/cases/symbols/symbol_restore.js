// @pending ["wait:s"]
// @restore
// @fulfill ["wait:s", 0]
// @fulfilled 7

async function run() {
  const fresh = Symbol("fresh");
  const registered = Symbol.for("k");
  const target = {};
  target[fresh] = 1;
  target[registered] = 2;
  target[Symbol.iterator] = 4;
  await wait("s");
  const again = Symbol.for("k");
  if (target[Symbol.for("nope")] !== undefined) return -1;
  if (target[Symbol("fresh")] !== undefined) return -1;
  if (Symbol.keyFor(registered) !== "k" || Symbol.keyFor(fresh) !== undefined) return -1;
  return target[fresh] + target[again] + target[Symbol.iterator];
}
