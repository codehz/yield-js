// @fulfilled true

export async function run() {
  let order = "";
  function key(mark) { order += mark; return "x"; }
  function rhs() { order += "r"; return 2; }
  let reads = 0, writes = 0;
  const pair = {
    n: 5,
    get [key("g")]() { throw "overwritten getter"; },
    set [key("s")](v) { writes = -10; },
    get x() { reads += 1; return this.n; },
    set x(v) { writes += 1; this.n = v; return 999; }
  };
  const result = (pair[key("c")] += rhs());
  if (order !== "gscr" || result !== 7 || pair.n !== 7 || reads !== 1 || writes !== 1) return false;

  const data = {set x(v) { throw "old setter"; }, [key("d")]: rhs()};
  if (data.x !== 2 || order !== "gscrdr") return false;
  const setterOnly = {get x() { return 99; }, x: 3, set x(v) { this.n = v; }};
  if (setterOnly.x !== undefined || (setterOnly.x = 8) !== 8 || setterOnly.n !== 8) return false;

  function C() {}
  C.prototype = {get x() { return this.n; }};
  const o = new C();
  o.n = 6;
  try {
    o.x = 0;
    return false;
  } catch (e) {
    if (typeof e !== "object" || e.name !== "TypeError" ||
        e.message !== "Cannot set property without a setter" || o.x !== 6) return false;
  }
  const throws = {set x(v) { throw v; }};
  try { throws.x = 4; } catch (e) { return e === 4; }
  return false;
}
