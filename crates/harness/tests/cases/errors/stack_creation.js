// @returns "ok"

function make(text) {
  return new Error(text);
}

export function run() {
  const first = make("first");
  /*😀é*/ const second = make("second"), third = make("third");
  const firstStack = first.stack;
  if (firstStack !== "Error: first\n    at make (main.js:4:10)\n    at run (main.js:8:17)") return firstStack;
  if (second.stack !== "Error: second\n    at make (main.js:4:10)\n    at run (main.js:9:26)") return second.stack;
  if (third.stack !== "Error: third\n    at make (main.js:4:10)\n    at run (main.js:9:50)") return third.stack;
  const descriptor = Object.getOwnPropertyDescriptor(first, "stack");
  if (descriptor.value !== firstStack || descriptor.enumerable || !descriptor.writable || !descriptor.configurable) {
    return "wrong descriptor";
  }
  try {
    try { throw first; } catch (e) { throw e; }
  } catch (e) {
    if (e !== first || e.stack !== firstStack) return "wrong rethrow stack";
  }
  return "ok";
}
