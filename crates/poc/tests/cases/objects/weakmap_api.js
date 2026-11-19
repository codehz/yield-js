// @returns true
export function run() {
  const key = {};
  const other = {};
  const value = { marker: 7 };
  const map = new WeakMap([[key, value]]);
  if (map.get(key) !== value || !map.has(key) || map.get(other) !== undefined) return false;
  if (map.set(key, other) !== map || map.get(key) !== other) return false;
  if (!map.delete(key) || map.delete(key) || map.has(key)) return false;
  map.set(key, value);
  if (map[Symbol.iterator] !== undefined || map.size !== undefined) return false;
  function throwsTypeError(f) {
    try { f(); } catch (e) { return e instanceof TypeError; }
    return false;
  }
  if (!throwsTypeError(() => WeakMap()) ||
      !throwsTypeError(() => new WeakMap([[1, value]]))) return false;
  const primitives = [null, undefined, true, 1, "x", Symbol("s"), Symbol.for("s")];
  for (const primitive of primitives) {
    if (!throwsTypeError(() => map.get(primitive)) ||
        !throwsTypeError(() => map.has(primitive)) ||
        !throwsTypeError(() => map.set(primitive, value)) ||
        !throwsTypeError(() => map.delete(primitive))) return false;
  }
  return true;
}
