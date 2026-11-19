// @returns 7

function outer() {
  const arrow = () => this.n;
  return arrow;
}

function run() {
  const obj = { n: 7, method: outer };
  return obj.method()();
}
