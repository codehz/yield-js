// @fulfilled "before:exec:after:done"

async function run() {
  let order = "before";
  const pending = new Promise(function (resolve) {
    order = order + ":exec";
    resolve("done");
  });
  order = order + ":after";
  const value = await pending;
  return order + ":" + value;
}
