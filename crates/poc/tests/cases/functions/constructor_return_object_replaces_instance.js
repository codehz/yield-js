// @returns "made"

function Box() {
  return { tag: "made" };
}

function run() {
  return new Box().tag;
}
