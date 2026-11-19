// @restore
// @fulfill ["wait:a",0]
// @fulfilled undefined

async function run(x) {
  await wait("a");
  return x;
}
