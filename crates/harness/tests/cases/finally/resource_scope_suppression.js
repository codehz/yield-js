// @returns true

export function run() {
  let trace = "";
  const key = Symbol.dispose;
  const ErrorType = SuppressedError;
  const a = { [key]() { trace += this === a ? "A" : "wrong"; throw "A"; } };
  const b = { [key]() { trace += "B"; throw "B"; } };
  try {
    using r = a, s = b;
    a[key] = () => { throw "changed"; };
    globalThis.SuppressedError = () => { throw "global"; };
    DisposableStack.prototype.dispose = () => { throw "stack"; };
    Array.prototype.pop = () => { throw "array"; };
    throw undefined;
  } catch (e) {
    if (!(e instanceof ErrorType) || e.error !== "A" || e.hasOwnProperty("message") ||
        !(e.suppressed instanceof ErrorType) || e.suppressed.error !== "B" ||
        !e.suppressed.hasOwnProperty("suppressed") || e.suppressed.suppressed !== undefined) return false;
  }
  if (trace !== "BA") return false;
  const marker = {};
  try {
    using first = { [key]() { trace += "1"; } }, second = { get [key]() { throw marker; } };
    return false;
  } catch (error) {
    if (error !== marker || trace !== "BA1") return false;
  }
  try {
    using last = { [key]() { trace += "2"; } };
    last = null;
  } catch (error) {
    return error instanceof TypeError && trace === "BA12";
  }
  return false;
}
