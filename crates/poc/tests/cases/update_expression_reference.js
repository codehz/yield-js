// @fulfilled true

async function run() {
  let log = "";
  let obj = {v: 8};
  function base() { log += "b"; return obj; }
  function key() { log += "k"; return "v"; }
  let before = base()[key()]--;
  let after = ++obj.v;
  return log === "bk" && before === 8 && after === 8 && obj.v === 8;
}
