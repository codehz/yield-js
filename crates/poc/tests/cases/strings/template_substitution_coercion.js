// @fulfilled true

async function run() {
  let order = "";
  function substitution(label, value) {
    order += "eval:" + label + ";";
    return {
      toString() { order += "string:" + label + ";"; return value; },
      valueOf() { order += "number:" + label + ";"; return 0; }
    };
  }

  if (`<${substitution("a", "A")}${substitution("b", "B")}>` !== "<AB>") return false;
  if (order !== "eval:a;string:a;eval:b;string:b;") return false;

  order = "";
  const symbol = Symbol("template");
  try {
    `${symbol}${substitution("late", "L")}`;
    return false;
  } catch (error) {
    if (error.name !== "TypeError" || order !== "") return false;
  }

  try {
    `${substitution("symbol", symbol)}${substitution("late", "L")}`;
    return false;
  } catch (error) {
    return error.name === "TypeError" && order === "eval:symbol;string:symbol;";
  }
}
