// @returns 2

function boom() {
  throw 6;
}
export function run() {
  try {
    boom();
  } catch {
    return 2;
  }
}
