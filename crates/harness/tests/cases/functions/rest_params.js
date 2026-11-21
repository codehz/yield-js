// @returns "2:2,3:1|1:5,undefined:4|0:undefined,undefined:6|0:undefined,undefined:undefined|true"

function f(a, ...rest) {
  return rest.length + ":" + rest[0] + "," + rest[1] + ":" + a;
}

function collect(...rest) {
  return rest;
}

export function run() {
  return f(1, 2, 3) + "|" + f(4, 5) + "|" + f(6) + "|" + f()
    + "|" + (collect() !== collect());
}
