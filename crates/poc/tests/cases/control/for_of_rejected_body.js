// @pending ["wait:body"]
// @restore
// @reject ["wait:body", "original"]
// @fulfilled "INVRoriginal"

async function run() {
  let trace = "";
  const iterator = {
    get next() {
      trace += this === iterator ? "N" : "bad";
      return function () {
        return { get value() { trace += "V"; return 1; }, done: 0 };
      };
    },
    get return() {
      trace += this === iterator ? "R" : "bad";
      throw "replacement";
    }
  };
  const iterable = {
    [Symbol.iterator]() {
      trace += this === iterable ? "I" : "bad";
      return iterator;
    }
  };
  // The protocol does not depend on the writable global Symbol.iterator property.
  Symbol.iterator = null;
  try {
    for (var value of iterable) {
      await wait("body");
    }
  } catch (error) {
    trace += error;
  }
  return trace;
}
