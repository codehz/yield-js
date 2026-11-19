// @restore
// @reject ["wait:a",5]
// @fulfilled 15

export async function run() {
  let base = 10;
  try {
    await wait("a");
    return 0;
  } catch (e) {
    return base + e;
  }
}
