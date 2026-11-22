// @fulfilled "ok"

function recurse(n) {
  if (n) return recurse(n - 1);
  if (+Symbol()) return "unreachable";
}

async function resumed() {
  await 0;
  return (() => new Error("later"))().stack;
}

function start() {
  return resumed();
}

export async function run() {
  try {
    recurse(2);
    return "missing TypeError";
  } catch (e) {
    const expected = "TypeError: Cannot convert a Symbol value to a number\n    at recurse (main.js:5:7)\n    at recurse (main.js:4:17)\n    at recurse (main.js:4:17)\n    at run (main.js:19:5)";
    if (!(e instanceof TypeError) || e.stack !== expected) return e.stack;
  }
  try {
    recurse(20);
    return "missing deep TypeError";
  } catch (e) {
    const frames = e.stack.split("\n");
    if (frames.length !== 11 || !frames[10].startsWith("    at recurse ")) return e.stack;
  }
  const stack = await start();
  if (stack !== "Error: later\n    at <anonymous> (main.js:10:17)\n    at resumed (main.js:10:10)") return stack;
  return "ok";
}
