// @fulfilled true

export async function run() {
  let o = { v: 1 };
  try {
    o.v &&= fail();
    return false;
  } catch (error) {
    return error === "boom" && o.v === 1;
  }
}

function fail() {
  throw "boom";
}
