// @returns true

let trace = "";
using a = { [Symbol.dispose]() { trace += "A/"; } };
{
  using b = { [Symbol.dispose]() { trace += "B/"; } };
  trace += "block/";
}
switch (1) {
  case 1: {
    using c = { [Symbol.dispose]() { trace += "C/"; } };
    trace += "case/";
    break;
  }
}
trace += "module/";

export function run() {
  return trace === "block/B/case/C/module/A/" && a !== undefined;
}
