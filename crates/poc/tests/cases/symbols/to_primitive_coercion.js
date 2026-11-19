// @fulfilled true

export async function run() {
  const key = Symbol("key");
  let hints = "";
  const value = {
    [Symbol.toPrimitive](hint) {
      hints += hint + ";";
      if (hint === "string") return key;
      return hint === "number" ? 7 : 9;
    },
    valueOf() { throw "ordinary fallback"; }
  };
  const target = { [value]: 3 };
  if (target[key] !== 3 || +value !== 7 || value + 1 !== 10 || !(value == 9)) return false;
  try {
    `${value}`;
    return false;
  } catch (error) {
    if (error.name !== "TypeError") return false;
  }
  if (hints !== "string;number;default;default;string;") return false;
  const fallback = { [Symbol.toPrimitive]: null, toString() { return "fallback"; } };
  if (`${fallback}` !== "fallback") return false;
  for (const bad of [1, function () { return {}; }]) {
    try {
      +{ [Symbol.toPrimitive]: bad, valueOf() { return 1; } };
      return false;
    } catch (error) {
      if (error.name !== "TypeError") return false;
    }
  }
  return true;
}
