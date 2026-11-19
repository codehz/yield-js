// @returns true

export function run() {
  let x = 4;
  const obj = { x: 10 };

  const a = (x)++;          // postfix: old value 4
  const b = ++(x);          // prefix: new value 6
  const c = (obj.x)++;      // postfix: old member value 10
  const d = --(obj.x);      // prefix: new member value 10
  const e = ((x))++;        // nested parens, postfix: old value 6
  const f = ++(((obj.x)));  // nested parens, prefix: new value 11

  return (
    a === 4 &&
    b === 6 &&
    c === 10 &&
    d === 10 &&
    e === 6 &&
    f === 11 &&
    x === 7 &&
    obj.x === 11
  );
}
