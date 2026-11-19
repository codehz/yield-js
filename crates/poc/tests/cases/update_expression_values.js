// @fulfilled true

async function run() {
  let x = "4";
  let a = x++;
  let b = --x;
  let c = ++x;
  let d = x--;
  return a === 4 && b === 4 && c === 5 && d === 5 && x === 4;
}
