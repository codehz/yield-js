// @returns "7:0:undefined|7:1:3|4:2:5"

function f(a = 7, ...rest) {
  return a + ":" + rest.length + ":" + rest[0];
}

export function run() {
  return f() + "|" + f(undefined, 3) + "|" + f(4, 5, 6);
}
