// @returns true
export function run() {
  if (Number.parseInt("\u00a0\t-0XfFtail") !== -255 || Number.parseInt(" +015rest", 0) !== 15) return false;
  if (Number.parseInt("0x10", 16) !== 16 || Number.parseInt("0x10", 10) !== 0) return false;
  if (Number.parseInt("1012", "2") !== 5 || Number.parseInt("Zz!", 36) !== 1295) return false;
  if (Number.parseInt("11", 4294967298) !== 3 || Number.parseInt("-11", 2.9) !== -3) return false;
  if (!Object.is(Number.parseInt("-0junk"), -0) || Number.parseInt(42) !== 42) return false;
  if (!Number.isNaN(Number.parseInt("0x")) || !Number.isNaN(Number.parseInt("xyz", 10))) return false;
  if (!Number.isNaN(Number.parseInt("1", 1)) || !Number.isNaN(Number.parseInt("1", 37))) return false;
  if (Number.parseFloat("\u00a0\t-.125e+2tail") !== -12.5 || Number.parseFloat(" +.5x") !== 0.5) return false;
  if (Number.parseFloat("1e+oops") !== 1 || Number.parseFloat("0x10") !== 0) return false;
  if (Number.parseFloat("+Infinitytail") !== Infinity || Number.parseFloat("-Infinity!") !== -Infinity) return false;
  if (!Object.is(Number.parseFloat("-0.00rest"), -0) || Number.parseFloat({toString() { return "8.5tail"; }}) !== 8.5) return false;
  if (!Number.isNaN(Number.parseFloat(".")) || !Number.isNaN(Number.parseFloat("  ")) || !Number.isNaN(Number.parseFloat("infinity"))) return false;
  let errors = 0;
  try { Number.parseInt(Symbol()); } catch (e) { if (e instanceof TypeError) errors++; }
  try { Number.parseFloat(Symbol()); } catch (e) { if (e instanceof TypeError) errors++; }
  return errors === 2;
}
