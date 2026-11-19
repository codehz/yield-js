// @returns true

function demo() {
  return 1;
}

function run() {
  const alias = demo;
  return alias === demo;
}
