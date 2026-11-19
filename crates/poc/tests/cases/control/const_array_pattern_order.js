// @returns true

export function run() {
  const first = 1;
  const second = 2;
  let assigned = 0;
  let trace = "";
  let next = 0;
  const source = {
    [Symbol.iterator]() { trace += "i"; return this; },
    next() {
      trace += "n";
      next++;
      const item = next;
      return { get value() { trace += "v"; return item + 9; } };
    },
    return() { trace += "c"; throw "close"; }
  };

  // Stop after reading the second value, preserving the first write and closing the iterator.
  try {
    [assigned, first, second] = source;
    return false;
  } catch (error) {
    if (!(error instanceof TypeError)) return false;
  }
  return trace === "invnvc" && next === 2 && assigned === 10 && first === 1 && second === 2;
}
