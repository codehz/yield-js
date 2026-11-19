// @fulfilled 3

async function run() {
  let n = 0;
  a: b: for (let i = 0; i < 3; i++) {
    do {
      n++;
      if (n > 4) return 99;
      if (i === 0) continue a;
      continue b;
    } while (false);
    return 98;
  }
  return n;
}
