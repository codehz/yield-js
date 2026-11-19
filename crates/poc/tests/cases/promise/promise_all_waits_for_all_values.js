// @pending ["wait:a"]
// @fulfill ["wait:a", 2]
// @fulfilled "2,3,4"

export async function run() {
  const values = await Promise.all([wait("a"), 3, Promise.resolve(4)]);
  return values[0] + "," + values[1] + "," + values[2];
}
