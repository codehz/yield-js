// @returns 3

function Box(value) {
  this.value = value;
  return value;
}

function run() {
  return new Box(3).value;
}
