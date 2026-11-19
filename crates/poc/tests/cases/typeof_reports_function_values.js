// @returns "function:function"

function run() {
  function f() {}
  const g = () => 1;
  return typeof f + ":" + typeof g;
}
