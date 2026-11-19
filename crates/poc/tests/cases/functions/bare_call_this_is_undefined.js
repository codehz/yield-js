// @returns true

function method() {
  return this === undefined;
}

export function run() {
  const call = method;
  return call();
}
