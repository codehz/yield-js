// @returns true
export function run() {
  const target = {};
  const source = { get value() { return 3; } };
  const symbol = Symbol("key");
  source[symbol] = 4;
  const assigned = Object.assign(target, null, source);
  if (assigned !== target || target.value !== 3 || target[symbol] !== 4) return false;
  if (!Object.is(0 / 0, 0 / 0) || Object.is(0, -0) || !Object.is(-0, -0)) return false;
  return Object.is({}, {}) === false && Object.is("x", "x");
}
