// @returns true

function outer(a = arguments[2], read = () => arguments[2]) {
  function inner() { return arguments[0]; }
  return a === 5 && read() === 5 && inner(7) === 7;
}

function shadow(arguments, value = arguments) { return value; }

export function run() {
  return outer(undefined, undefined, 5) && shadow(31) === 31;
}
