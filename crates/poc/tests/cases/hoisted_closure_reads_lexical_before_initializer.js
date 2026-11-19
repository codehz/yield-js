// @fulfilled true

async function run() {
  function read() {
    return x;
  }
  let before = read() === undefined;
  let x = 7;
  let after = read() === 7;
  return before && after;
}
