// @fulfilled "catch:boom:finally:value"

async function run() {
  let order = "";
  const value = await Promise.reject("boom").catch(function (reason) {
    order = order + "catch:" + reason;
    return "value";
  }).finally(function () {
    order = order + ":finally";
  });
  return order + ":" + value;
}
