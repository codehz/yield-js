// @returns 120

export function run() {
  const fact = (n) => n <= 1 ? 1 : n * fact(n - 1);
  return fact(5);
}
