// @returns "own"

function Box() {
  this.kind = "own";
}

function run() {
  Box.prototype.kind = "proto";
  return new Box().kind;
}
