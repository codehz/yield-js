// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a",2]
// @fulfilled 2

async function run() {
  return await wait("a");
}
