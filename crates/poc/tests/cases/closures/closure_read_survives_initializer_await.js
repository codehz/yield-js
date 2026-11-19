// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 7]
// @fulfilled true

export async function run() {
  let read = () => x;
  let before = read();
  let x = await wait("a");
  let after = read();
  return before === undefined && after === 7;
}
