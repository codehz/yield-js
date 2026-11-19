// @pending ["wait:a", "wait:b"]
// @fulfill ["wait:b", 5]
// @fulfilled 5

async function run() {
  return await Promise.race([wait("a"), wait("b")]);
}
