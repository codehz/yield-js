// @returns 31

function make() {
  let n = 0;
  return function() {
    n = n + 1;
    return n;
  };
}
export function run() {
  const a = make();
  a();
  a();
  const b = make();
  return a() * 10 + b();
}
