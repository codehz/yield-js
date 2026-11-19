// @returns true

function f(a) {
  a = 9;
  const original = arguments[0];
  arguments[0] = 7;
  return [arguments, original, a];
}

export function run() {
  const first = f(2);
  const second = f(4);
  first[0][0] = 11;
  return first[0] !== second[0] && first[1] === 2 && second[1] === 4
    && first[2] === 9 && second[2] === 9 && second[0][0] === 7;
}
