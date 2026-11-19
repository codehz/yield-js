// @rejected "boom"

function fail() {
  throw "boom";
}

export async function run() {
  return delete fail();
}
