// @returns true
export function run() {
  if (typeof Math !== "object" || Object.getPrototypeOf(Math) !== Object.prototype
      || Math.prototype !== undefined || Object.keys(Math).length !== 0) return false;
  const constants = {
    E: 2.718281828459045, LN10: 2.302585092994046, LN2: 0.6931471805599453,
    LOG10E: 0.4342944819032518, LOG2E: 1.4426950408889634, PI: 3.141592653589793,
    SQRT1_2: 0.7071067811865476, SQRT2: 1.4142135623730951
  };
  for (const key of Object.keys(constants)) {
    const d = Object.getOwnPropertyDescriptor(Math, key);
    if (d.value !== constants[key] || d.writable || d.enumerable || d.configurable) return false;
  }
  try { Math.PI = 0; return false; } catch (e) { if (!(e instanceof TypeError)) return false; }
  try { Math(); return false; } catch (e) { if (!(e instanceof TypeError)) return false; }
  try { new Math(); return false; } catch (e) { return e instanceof TypeError; }
}
