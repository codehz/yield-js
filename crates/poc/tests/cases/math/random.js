// @returns true
export function run() {
  const ignored = { valueOf() { throw 'random ignores arguments'; } };
  let previous = Math.random(ignored);
  if (!(previous >= 0 && previous < 1)) return false;
  for (let i = 0; i < 32; i++) {
    const next = Math.random();
    if (!(next >= 0 && next < 1) || next === previous) return false;
    previous = next;
  }
  return true;
}
