// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 7]
// @fulfilled "1,2,7,8,9"

export async function run() {
  const pending = [8, 9].values();
  const partial = [1, ...[2]];
  const extra = await wait("a");
  const values = [...partial, extra, ...pending];
  return values[0] + "," + values[1] + "," + values[2] + "," + values[3] + "," + values[4];
}
