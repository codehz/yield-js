// @fulfilled 31

function top() { return 2; }

async function run() {
  let o = { x: 7, m() { return this.x; }, detached() { return this === undefined; } };
  let f = null;
  if (f?.() !== undefined || top?.() !== 2) return -1;
  if ((o?.m)?.() !== 7 || !(0, o?.detached)()) return -1;
  return o?.m() + o.m?.() + (o?.m)() + seed?.();
}
