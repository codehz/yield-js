// @rejected "TypeError: value is not a constructor"

async function run() {
  const arrow = () => 1;
  return new arrow();
}
