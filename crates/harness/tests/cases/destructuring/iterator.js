// @fulfilled "INTNVSRC|IR|INVRoriginal|INstep|INVNVN:1:2"

export async function run() {
  let trace = "";
  function iterable(mode) {
    return {
      [Symbol.iterator]() {
        trace += "I";
        let index = 0;
        return {
          next() {
            trace += "N";
            if (mode === "step") throw "step";
            const current = index++;
            if (current === 2) return { done: true };
            return {
              done: false,
              get value() { trace += "V"; return mode === "abrupt" ? undefined : current + 1; },
            };
          },
          return() {
            trace += "R";
            if (mode === "abrupt") throw "replacement";
            return {};
          },
        };
      },
    };
  }
  function fail() { throw "original"; }
  const target = { set item(value) { trace += "S"; this.saved = value; } };
  [, (trace += "T", target).item] = iterable("early");
  trace += target.saved === 2 ? "C|" : "wrong|";
  const [] = iterable("empty");
  trace += "|";
  try {
    const [value = fail()] = iterable("abrupt");
  } catch (error) {
    trace += error;
  }
  trace += "|";
  try {
    const [value] = iterable("step");
  } catch (error) {
    trace += error;
  }
  trace += "|";
  const [first, ...remaining] = iterable("exhausted");
  return trace + ":" + first + ":" + remaining[0];
}
