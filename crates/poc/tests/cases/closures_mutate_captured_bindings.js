// @returns 35

function run() {
  let x = 1;
  function inc() {
    x = x + 2;
    return x;
  }
  const first = inc();
  inc();
  return first * 10 + x;
}
