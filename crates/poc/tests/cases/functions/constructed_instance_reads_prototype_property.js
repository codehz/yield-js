// @returns "box"

function Box() {}

export function run() {
  Box.prototype.kind = "box";
  return new Box().kind;
}
