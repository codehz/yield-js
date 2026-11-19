// @restore
// @fulfill ["wait:a",0]
// @fulfilled undefined

export async function run(x) {
  await wait("a");
  return x;
}
