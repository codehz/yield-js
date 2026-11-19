// @returns true

export function run() {
  const value = "é😀z😀";
  if (value.charAt(0) !== "é" || value.charAt(99) !== "" || value.charCodeAt(1) !== 55357) return false;
  if (!Object.is(value.charCodeAt(-1), NaN) || value.charCodeAt(2) !== 56832) return false;
  if (value.codePointAt(1) !== 128512 || value.codePointAt(2) !== 56832 || value.codePointAt(-1) !== undefined) return false;
  if (value.at(-3) !== "z" || value.at(6) !== undefined) return false;
  if (value.indexOf("😀", 2) !== 4 || value.lastIndexOf("😀", 3) !== 1 || value.lastIndexOf("😀") !== 4) return false;
  if (!value.includes("😀", 2) || value.includes("é", 1) || value.indexOf("x") !== -1) return false;
  if (!value.startsWith("😀", 1) || value.startsWith("😀") || !value.endsWith("😀", 3) || value.endsWith("z")) return false;
  if (value.slice(-5, -3) !== "😀" || value.slice(3, 1) !== "" || value.substring(3, 1) !== "😀") return false;
  if (value.substring(-2, 1) !== "é") return false;
  let errors = 0;
  try { value[1]; } catch (error) { if (error.name === "TypeError") errors++; }
  try { value.charAt(1); } catch (error) { if (error.name === "TypeError") errors++; }
  try { value.at(-1); } catch (error) { if (error.name === "TypeError") errors++; }
  try { value.slice(0, 2); } catch (error) { if (error.name === "TypeError") errors++; }
  try { value.substring(1, 2); } catch (error) { if (error.name === "TypeError") errors++; }
  try { value.split(""); } catch (error) { if (error.name === "TypeError") errors++; }
  return errors === 6;
}
