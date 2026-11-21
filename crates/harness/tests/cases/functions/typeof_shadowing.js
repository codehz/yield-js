// @returns "function:number:undefined:function"

function f() {}

export function run() {
  let r = typeof f;
  {
    // A block-scoped shadow reports the runtime value of the inner binding.
    let f = 7;
    r += ":" + typeof f;
  }
  // Undeclared names still fold to "undefined" without evaluating.
  r += ":" + typeof definitelyNotDeclaredHere;
  // The top-level function itself still reports "function".
  r += ":" + typeof f;
  return r;
}
