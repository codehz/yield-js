// @pending ["wait:step"]
// @restore
// @fulfill ["wait:step", 0]
// @pending ["wait:finally"]
// @restore
// @fulfill ["wait:finally", 0]
// @fulfilled "123C"

export async function run() {
  let trace = "";
  const array = [1, 2];
  const iterator = array.values();
  iterator.return = function () {
    trace += this === iterator ? "C" : "wrong receiver";
    return {};
  };
  outer: for (const item of iterator) {
    trace += item;
    if (item === 1) {
      await wait("step");
      array[2] = 3;
      iterator.next = null;
      continue outer;
    }
    if (item === 3) {
      try {
        break outer;
      } finally {
        await wait("finally");
      }
    }
  }
  return trace;
}
