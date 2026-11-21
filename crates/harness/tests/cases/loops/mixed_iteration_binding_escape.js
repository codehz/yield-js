// @returns "0,111|10"

export function run() {
  const captured = [];
  let last = -1;
  for (let i = 0, j = 0; i < 2; i += 1, j += 10) {
    const scaled = j + i;
    captured.push(() => i * 100 + scaled);
    last = j;
  }
  return `${captured.map((fn) => fn()).join(",")}|${last}`;
}
