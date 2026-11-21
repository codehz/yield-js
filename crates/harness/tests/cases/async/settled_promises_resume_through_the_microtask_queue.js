// @fulfilled 6

async function inner() {
  return 5;
}
export async function run() {
  const x = await inner();
  return x + 1;
}
