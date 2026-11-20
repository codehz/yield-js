// @returns true

export function run() {
  let trace = "";
  const value = {
    valueOf() {
      trace += "v";
      return 2;
    }
  };
  const original = value;
  function rhs() {
    trace += "r";
    return 3;
  }

  try { value += rhs(); return false; }
  catch (error) { if (!(error instanceof TypeError) || trace !== "rv") return false; }
  try { value++; return false; }
  catch (error) { if (!(error instanceof TypeError) || trace !== "rvv") return false; }
  try { --value; return false; }
  catch (error) { if (!(error instanceof TypeError) || trace !== "rvvv") return false; }
  return value === original;
}
