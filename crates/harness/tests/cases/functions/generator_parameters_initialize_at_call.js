// @returns "1:0:6:false:1"

export function run() {
  let defaults = 0;
  let bodyRuns = 0;
  function* g(x = (defaults = defaults + 1) * 6) {
    bodyRuns = bodyRuns + 1;
    yield x;
  }
  const it = g();
  const atCall = defaults;
  const bodyAtCall = bodyRuns;
  const step = it.next();
  return [atCall, bodyAtCall, step.value, step.done, bodyRuns].join(":");
}
