// @returns 3

function Box(value) {
  this.value = value;
  return value;
}

export function run() {
  return new Box(3).value;
}
