// @fulfilled 1

export async function run() {
  let n = 0;
  let o = null;
  try {
    (o?.m)(++n);
  } catch (e) {}
  try {
    false?.();
    return -1;
  } catch (e) {}
  return n;
}
