// @returns true

function method() {
  return this === undefined;
}

function run() {
  const call = method;
  return call();
}
