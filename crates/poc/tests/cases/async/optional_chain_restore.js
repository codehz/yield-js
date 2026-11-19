// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 2]
// @fulfilled 7

async function run() {
  let o = { x: 5, m(v) { return this.x + v; } };
  return o.m?.(await wait('a'));
}
