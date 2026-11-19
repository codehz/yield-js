// @rejected "boom"

function fail() {
  throw "boom";
}

async function run() {
  return delete fail();
}
