// @pending ["wait:s"]
// @restore
// @reject ["wait:s", "boom"]
// @fulfilled "symbol|truthy|TypeError: Symbol is not a constructor|TypeError: Symbol.keyFor requires a Symbol|TypeError: Cannot convert a Symbol value to a string|TypeError: Cannot convert a Symbol value to a number|g8|same|distinct"

export async function run() {
  const s = Symbol("s");
  let out = typeof s + "|" + (s ? "truthy" : "falsy");
  try {
    new Symbol("x");
  } catch (e) {
    out = out + "|" + describeError(e);
  }
  try {
    Symbol.keyFor(1);
  } catch (e) {
    out = out + "|" + describeError(e);
  }
  try {
    "v" + s;
  } catch (e) {
    out = out + "|" + describeError(e);
  }
  try {
    +s;
  } catch (e) {
    out = out + "|" + describeError(e);
  }
  let guarded = 7;
  try {
    await wait("s");
  } catch (e) {
    guarded = guarded + 1;
  }
  out = out + "|g" + guarded;
  out = out + "|" + (s === s ? "same" : "different");
  out = out + "|" + (s === Symbol("s") ? "equal" : "distinct");
  return out;
}

function describeError(e) {
  if (typeof e !== "object") return "not an error object";
  const description = e.name + ": " + e.message;
  return e.stack === description ? description : "wrong stack";
}
