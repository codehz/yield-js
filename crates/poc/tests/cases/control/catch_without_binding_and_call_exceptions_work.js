// @returns 2

function boom() {
  throw 6;
}
function run() {
  try {
    boom();
  } catch {
    return 2;
  }
}
