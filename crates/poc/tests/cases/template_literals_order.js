// @fulfilled true

async function run() {
  let i = 1;
  return `${i++}${i++}
${null}` === "12\nnull" && `` === "";
}
