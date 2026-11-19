// @fulfill ["wait:a",1]
// @rejected "sync boom"

function fail() {
  throw "sync boom";
}
export async function run() {
  await wait("a");
  fail();
  return 1;
}
