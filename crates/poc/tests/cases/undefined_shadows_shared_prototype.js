// @returns true

function Box() {}

function run() {
  Box.prototype.x = 7;
  let first = new Box();
  let second = new Box();
  first.x = undefined;
  return first.x === undefined && second.x === 7 &&
    first.missing === undefined && second.missing === undefined;
}
