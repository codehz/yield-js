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
  const high = "\uD83D";
  const low = "\uDE00";
  const units = value.split("");
  return value[1] === high && value[6] === undefined &&
    value.charAt(1) === high && value.at(-1) === low &&
    value.slice(0, 2) === "é" + high && value.substring(1, 2) === high &&
    units.length === 6 && units[1] === high && units[2] === low && units.join("") === value;
}
