// @fulfilled true

async function run() {
  try {
    await callWithoutNew();
  } catch (e) {
    return typeof e === "object" && e.name === "TypeError" && e.message === "Promise constructor requires new";
  }
  return false;
}

async function callWithoutNew() {
  return Promise(function () {});
}
