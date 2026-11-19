// @returns "made"

function Box() {
  return { tag: "made" };
}

export function run() {
  return new Box().tag;
}
