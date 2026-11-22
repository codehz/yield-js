// @returns "RELR|EL|4:boom"

export function run() {
  let trace = "";
  let value = 2 + 2;
  const left = { valueOf() { trace += "L"; return 5; } };
  const right = { valueOf() { trace += "R"; return 3; } };
  function evaluateRight() { trace += "RE"; return right; }
  left - evaluateRight();
  trace += "|";
  function evaluatePrimitive() { trace += "E"; return 3; }
  const rhs = 1 + 2;
  left - (evaluatePrimitive(), rhs);
  trace += "|";
  const bad = { valueOf() { throw "boom"; } };
  try {
    value = bad - rhs;
  } catch (error) {
    trace += `${value}:${error}`;
  }
  return trace;
}
