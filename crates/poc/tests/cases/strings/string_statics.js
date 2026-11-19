// @returns true

export function run() {
  if (String.fromCharCode(233, 55357, 56832, 65536 + 122) !== "é😀z") return false;
  if (String.fromCodePoint(233, 128512, 122) !== "é😀z") return false;
  if (String.fromCharCode() !== "" || String.fromCodePoint() !== "") return false;
  if (String.raw({raw: ["a\\n", "b", "c"]}, 7) !== "a\\n7bc") return false;
  if (String.raw({raw: {0: "a", 1: 3, length: 2}}, false, "ignored") !== "afalse3") return false;
  if (String.raw({raw: []}) !== "") return false;
  let errors = 0;
  try { String.fromCharCode(55357); } catch (error) { if (error.name === "TypeError") errors++; }
  try { String.fromCodePoint(56832); } catch (error) { if (error.name === "TypeError") errors++; }
  try { String.fromCodePoint(1114112); } catch (error) { if (error.name === "RangeError") errors++; }
  try { String.fromCodePoint(1.5); } catch (error) { if (error.name === "RangeError") errors++; }
  try { String.raw({}); } catch (error) { if (error.name === "TypeError") errors++; }
  return errors === 5;
}
