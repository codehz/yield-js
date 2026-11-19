// @returns true

export function run() {
  if ("éß".toUpperCase() !== "ÉSS" || "É".toLowerCase() !== "é") return false;
  const spaced = "\uFEFF\t a \n";
  if (spaced.trim() !== "a" || spaced.trimStart() !== "a \n" || spaced.trimEnd() !== "\uFEFF\t a") return false;
  const parts = "a::b:c".split(":", 3);
  if (parts.length !== 3 || parts[0] !== "a" || parts[1] !== "" || parts[2] !== "b") return false;
  if ("ab".split(undefined)[0] !== "ab" || "ab".split(":", 0).length !== 0 || "ab".split("")[1] !== "b") return false;
  if ("a".concat(2, null) !== "a2null" || "é".repeat(2.9) !== "éé" || "a".repeat(0) !== "") return false;
  if ("é".padStart(4, "ab") !== "abaé" || "é".padEnd(3) !== "é  " || "é".padEnd(4, "") !== "é") return false;
  if ("abcabc".replace("b", "$$:$&:$`:$'") !== "a$:b:a:cabccabc") return false;
  if ("😀x😀".replace("x", (match, index, input) => match + index + input.length) !== "😀x25😀") return false;
  if ("abc".replace("x", () => { throw "unmatched"; }) !== "abc") return false;
  let errors = 0;
  try { "x".repeat(-1); } catch (error) { if (error.name === "RangeError") errors++; }
  try { "x".padStart(2, "😀"); } catch (error) { if (error.name === "TypeError") errors++; }
  return errors === 2;
}
