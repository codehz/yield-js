// @fulfilled true

async function run() {
  let n = 0;
  let v = null?.[++n].m(++n);
  let u = undefined?.(++n);
  return v === undefined && u === undefined && n === 0;
}
