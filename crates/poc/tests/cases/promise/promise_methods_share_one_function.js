// @returns true

function run() {
  const left = Promise.resolve(1);
  const right = Promise.resolve(2);
  return left.then === right.then && left.then === Promise.prototype.then;
}
