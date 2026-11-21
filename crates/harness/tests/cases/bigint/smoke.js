// @returns "bigint:3:1267650600228229401496703205376:2:1"

export function run() {
  const big = 2n ** 100n;
  return [
    typeof 1n,
    1n + 2n,
    big.toString(),
    7n / 3n,
    7n % 3n,
  ].join(":");
}
