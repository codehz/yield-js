// @returns 17

function f(...[a, b]) {
  return a + b;
}

export function run() {
  const arrow = (...[x, y]) => x * y;
  return f(2, 3, 100) + arrow(3, 4);
}
