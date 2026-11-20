// @returns true

export function run() {
  const truthy = 7;
  const falsy = 0;
  const nil = null;
  let trace = "";
  function rhs(mark) {
    trace += mark;
    return 9;
  }

  if ((truthy ||= rhs("skip")) !== 7) return false;
  if ((falsy &&= rhs("skip")) !== 0) return false;
  if ((falsy ??= rhs("skip")) !== 0) return false;
  if (trace !== "") return false;

  try { truthy &&= rhs("a"); return false; }
  catch (error) { if (!(error instanceof TypeError) || trace !== "a") return false; }
  try { falsy ||= rhs("o"); return false; }
  catch (error) { if (!(error instanceof TypeError) || trace !== "ao") return false; }
  try { nil ??= rhs("n"); return false; }
  catch (error) { if (!(error instanceof TypeError) || trace !== "aon") return false; }
  return truthy === 7 && falsy === 0 && nil === null;
}
