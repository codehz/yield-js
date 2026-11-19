// @fulfill ["wait:a",1]
// @rejected "sync boom"

function fail() {
  throw "sync boom";
}
async function run() {
  await wait("a");
  fail();
  return 1;
}
