// @returns true

export function run() {
  let count = 0;
  const value = String("é😀z");
  if (typeof value !== "string" || String() !== "" || String(undefined) !== "undefined") return false;
  try { String(Symbol("tag")); } catch (error) { if (error.name === "TypeError") count++; }
  if (value.length !== 4 || value[0] !== "é" || value["3"] !== "z") return false;
  if (value[4] !== undefined || value[-1] !== undefined || value["03"] !== undefined || value.missing !== undefined) return false;
  if (value.constructor !== String || value.toString() !== value || value.valueOf() !== value) return false;
  if (value[Symbol.iterator] !== String.prototype[Symbol.iterator]) return false;

  try { value.length = 0; } catch (error) { if (error.name === "TypeError") count++; }
  try { value[0] = "x"; } catch (error) { if (error.name === "TypeError") count++; }
  try { value.extra = true; } catch (error) { if (error.name === "TypeError") count++; }
  return count === 4 && value.length === 4 && value[0] === "é" && value.extra === undefined;
}
