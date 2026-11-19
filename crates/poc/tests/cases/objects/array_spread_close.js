// @fulfilled "NRoriginal"

export async function run() {
  let trace = "";
  const iterable = {
    [Symbol.iterator]() {
      return {
        next() {
          trace += "N";
          throw "original";
        },
        return() {
          trace += "R";
          throw "replacement";
        },
      };
    },
  };
  try {
    const values = [1, ...iterable, 2];
    trace += values;
  } catch (error) {
    trace += error;
  }
  return trace;
}
