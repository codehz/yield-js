// @fulfilled true

async function run() {
  function callable() {}
  const text = `${callable}`;
  if (typeof text !== "string" || callable + "" !== text || `${callable}` !== text) return false;
  const numeric = +callable;
  if (numeric === numeric) return false;

  let calls = "";
  callable.valueOf = function () { calls += "v"; return this === callable ? 6 : 0; };
  callable.toString = function () { calls += "s"; return this === callable ? "fn" : "wrong"; };
  if (+callable !== 6 || callable + 1 !== 7 || `${callable}` !== "fn") return false;

  callable.valueOf = function () { calls += "o"; return this; };
  if (callable + "!" !== "fn!" || calls !== "vvsos") return false;
  callable.toString = function () { calls += "t"; return this; };
  try {
    `${callable}`;
    return false;
  } catch (error) {
    return error.name === "TypeError" && calls === "vvsosto";
  }
}
