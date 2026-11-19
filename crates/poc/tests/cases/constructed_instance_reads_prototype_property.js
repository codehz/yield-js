// @returns "box"

function Box() {}

function run() {
  Box.prototype.kind = "box";
  return new Box().kind;
}
