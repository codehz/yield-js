// @fulfilled true

async function run() {
  try {
    await constructArrow();
  } catch (e) {
    return typeof e === "object" && e.name === "TypeError" && e.message === "value is not a constructor";
  }
  return false;
}

async function constructArrow() {
  const arrow = () => 1;
  return new arrow();
}
