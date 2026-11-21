// @fulfilled 4

export async function run() {
  let i = 0;
  let n = 0;
  d: do {
    if (++n > 4) return 99;
    continue d;
  } while (++i < 2);
  w: while (++i < 5) {
    n++;
    if (n > 6) return 98;
    continue w;
  }
  return n;
}
