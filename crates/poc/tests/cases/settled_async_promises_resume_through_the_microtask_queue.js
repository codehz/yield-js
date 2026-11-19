// @fulfilled 6

async function inner() {
  return 5;
}
async function run() {
  const x = await inner();
  return x + 1;
}
