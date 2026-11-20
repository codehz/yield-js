// @returns true

export function run() {
  let a = [1, , 2];
  let checks = a.length === 3;
  checks &= !(1 in a);
  checks &= a[1] === undefined;
  checks &= a.map((value) => value === undefined ? 0 : value * 10).length === 3;
  checks &= a.map((value, index) => index in a).join() === "true,,true";
  let seen = 0;
  a.forEach(() => { seen += 1; });
  checks &= seen === 2;
  checks &= JSON.stringify(a) === "[1,null,2]";
  checks &= a.join() === "1,,2";
  return checks === 1;
}
