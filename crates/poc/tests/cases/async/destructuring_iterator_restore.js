// @pending ["wait:fulfill"]
// @restore
// @fulfill ["wait:fulfill", 7]
// @pending ["wait:reject"]
// @restore
// @reject ["wait:reject", "boom"]
// @fulfilled "INNVR7:8|INRboom"

export async function run() {
  let trace = "";
  const iterable = {
    [Symbol.iterator]() {
      trace += "I";
      let index = 0;
      return {
        next() {
          trace += "N";
          if (index++ === 0) return { done: false, value: undefined };
          return { done: false, get value() { trace += "V"; return 8; } };
        },
        return() { trace += "R"; return {}; },
      };
    },
  };

  const [first = await wait("fulfill"), second] = iterable;
  trace += first + ":" + second + "|";
  try {
    const [first = await wait("reject"), second] = iterable;
    trace += first + ":" + second;
  } catch (error) {
    trace += error;
  }
  return trace;
}
