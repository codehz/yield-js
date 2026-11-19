// @returns true

function demo() {
  return 1;
}

export function run() {
  const alias = demo;
  return alias === demo;
}
