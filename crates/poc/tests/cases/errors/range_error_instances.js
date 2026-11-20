// @returns true
export function run() {
  let count = 0;
  try { "x".repeat(-1); } catch (error) { if (error instanceof RangeError && error.name === "RangeError") count++; }
  try { String.fromCodePoint(0x110000); } catch (error) { if (error instanceof RangeError && error.name === "RangeError") count++; }
  return count === 2;
}
