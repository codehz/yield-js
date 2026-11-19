// @returns true

export function run() {
  let leading = [, 7, 8];
  let trailing = [7, 8, ];
  let multi = [1, , , 4];
  let checks = leading.length === 3 && !(0 in leading);
  checks &= leading[0] === undefined;
  checks &= trailing.length === 2 && !(2 in trailing);
  checks &= multi.length === 4 && !(1 in multi) && !(2 in multi);
  checks &= Array(3).length === 3;
  checks &= !(0 in Array(3));
  checks &= [...multi][3] === 4;
  let spread = [...multi];
  checks &= spread.length === 4 && 1 in spread && spread[1] === undefined;
  return checks === 1;
}
