// @pending ["wait:cleanup"]
// @restore
// @fulfill ["wait:cleanup", 8]
// @fulfilled true

export async function run() {
  const p = Promise.resolve(1);
  const marker = {};
  let result;
  let argsOK = false;
  p.then = function (ok, bad) {
    argsOK = this === p && typeof ok === "function" && typeof bad === "function";
    result = ok(42);
    return marker;
  };
  let cleaned = false;
  const returned = p.finally(async function () {
    await wait("cleanup");
    cleaned = true;
  });
  const value = await result;
  p.then = (ok, bad) => ok === 7 && bad === 7;
  let reason;
  try { await Promise.reject("original").finally(() => Promise.resolve(0)); }
  catch (error) { reason = error; }
  return argsOK && returned === marker && cleaned && value === 42 && p.finally(7)
    && (await Promise.resolve(9).finally(null)) === 9 && reason === "original";
}
