// @returns true
export function run() {
  let order = '';
  const marker = {};
  const a = { valueOf() { order += 'a'; return '2'; } };
  const b = { valueOf() { order += 'b'; return 3; } };
  const bad = { valueOf() { order += 'x'; throw marker; } };
  if (Math.pow(a, b, bad) !== 8 || order !== 'ab') return false;
  if (!Number.isNaN(Math.max(NaN, b, a)) || order !== 'abba') return false;
  try { Math.hypot(Infinity, a, bad, b); return false; }
  catch (e) { if (e !== marker || order !== 'abbaax') return false; }
  try { Math.imul(bad, b); return false; }
  catch (e) { if (e !== marker || order !== 'abbaaxx') return false; }
  try { Math.abs(Symbol()); return false; }
  catch (e) { if (!(e instanceof TypeError)) return false; }
  try { Math.min(NaN, Symbol()); return false; }
  catch (e) { return e instanceof TypeError; }
}
