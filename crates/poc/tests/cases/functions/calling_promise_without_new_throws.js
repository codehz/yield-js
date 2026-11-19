// @rejected "TypeError: Promise constructor requires new"

async function run() {
  return Promise(function () {});
}
