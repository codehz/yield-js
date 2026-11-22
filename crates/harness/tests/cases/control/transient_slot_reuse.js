// @args [3, 5]
// @returns 26

function raise() {
  throw 99;
}

export function run(count, step) {
  let total = 0;
  for (let i = 0; i < count; i++) {
    const kept = step + i;
    try {
      if (i === 1) raise();
      total += kept * 2;
    } catch {
      total += kept - 4;
    }
  }
  return total;
}
