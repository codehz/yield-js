// @fulfilled true

async function run() {
  const value = { valueOf() { return 7; } };
  let caught = false;
  try {
    ({ valueOf() { throw new TypeError("conversion failed"); } }) - 1;
  } catch (error) {
    caught = error.name === "TypeError";
  }
  return value + 1 === 8 && +{} !== +{} && caught;
}
