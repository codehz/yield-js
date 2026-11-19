// @returns 94

export function run() {
  let side = 0;
  try {
    try {
      throw 4;
    } finally {
      side = 9;
    }
  } catch (e) {
    return side * 10 + e;
  }
}
