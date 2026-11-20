// @returns "own"

function Box() {
  this.kind = "own";
}

export function run() {
  Box.prototype.kind = "proto";
  return new Box().kind;
}
