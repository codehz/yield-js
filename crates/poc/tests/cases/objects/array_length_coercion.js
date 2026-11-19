// @fulfilled true

export async function run() {
  const a = [0, 1, 2, 3];
  let calls = 0;
  const len = { valueOf() { calls++; return "2"; } };
  a.length = len;
  if (calls !== 2 || a.length !== 2 || 2 in a) return false;
  Object.defineProperties(a, { length: { value: len } });
  if (calls !== 4 || a.length !== 2) return false;
  if (!Reflect.set(a, "length", len) || calls !== 6) return false;

  const different = { valueOf() { calls++; return calls === 7 ? 1 : 2; } };
  try {
    Reflect.defineProperty(a, "length", { value: different });
    return false;
  } catch (e) {
    if (e.name !== "RangeError" || calls !== 8 || a.length !== 2) return false;
  }

  const marker = {};
  const throwing = {
    [Symbol.toPrimitive](hint) {
      if (hint !== "number") throw "wrong hint";
      calls++;
      if (calls === 10) throw marker;
      return 1;
    }
  };
  try {
    Object.defineProperty(a, "length", { value: throwing });
    return false;
  } catch (e) {
    if (e !== marker || calls !== 10 || a.length !== 2) return false;
  }
  try { a.length = Symbol(); return false; } catch (e) { return e.name === "TypeError"; }
}
