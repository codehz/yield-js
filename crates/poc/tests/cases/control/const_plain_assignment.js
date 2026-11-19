// @returns true

export function run() {
  const value = 7;
  let calls = 0;
  function rhs() {
    calls++;
    return 8;
  }

  try {
    value = rhs();
    return false;
  } catch (error) {
    if (!(error instanceof TypeError) || error.message !== "Assignment to constant variable.") return false;
  }
  try {
    value = (function () { throw "rhs"; })();
    return false;
  } catch (error) {
    if (error !== "rhs") return false;
  }
  return value === 7 && calls === 1;
}
