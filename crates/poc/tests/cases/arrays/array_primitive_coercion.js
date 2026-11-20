// @fulfilled true

export async function run() {
  const sparse = [1, , null, undefined, 5];
  if (sparse.join("|") !== "1||||5" || `${sparse}` !== "1,,,,5") return false;
  if (+[7] !== 7 || [7] + 1 !== "71") return false;

  const cyclic = [1];
  cyclic[1] = cyclic;
  cyclic[2] = 3;
  if (cyclic.join("|") !== "1||3" || `${cyclic}` !== "1,,3") return false;

  let reads = 0;
  const withGetter = [0, 2];
  Object.defineProperty(withGetter, "0", {
    get() {
      reads++;
      this[1] = 8;
      this[2] = 9;
      return 7;
    }
  });
  if (`${withGetter}` !== "7,8" || reads !== 1) return false;

  const custom = [4];
  let calls = "";
  custom.valueOf = function () { calls += "v"; return this[0]; };
  custom.join = function () { calls += "j"; return "joined:" + this[0]; };
  if (+custom !== 4 || custom + 1 !== 5 || `${custom}` !== "joined:4") return false;
  custom.toString = function () { calls += "s"; return "overridden"; };
  return `${custom}` === "overridden" && calls === "vvjs";
}
