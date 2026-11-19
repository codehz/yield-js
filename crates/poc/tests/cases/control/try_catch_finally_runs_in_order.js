// @returns 108

export function run() {
  let out = 0;
  try {
    out = 1;
    throw 7;
  } catch (e) {
    out = out + e;
  } finally {
    out = out + 100;
  }
  return out;
}
