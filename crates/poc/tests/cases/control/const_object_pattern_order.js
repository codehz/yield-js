// @returns true

export function run() {
  const first = 1;
  const second = 2;
  let trace = "";
  const target = { set value(value) { trace += value; } };
  function receiver() { trace += "m"; return target; }
  const source = {
    a: "s",
    nested: {
      get b() { trace += "b"; return undefined; },
      get c() { trace += "c"; return 4; }
    }
  };
  function fallback() { trace += "d"; return 3; }

  try {
    ({ a: receiver().value, nested: { b: first = fallback(), c: second } } = source);
    return false;
  } catch (error) {
    if (!(error instanceof TypeError)) return false;
  }
  return trace === "msbd" && first === 1 && second === 2;
}
