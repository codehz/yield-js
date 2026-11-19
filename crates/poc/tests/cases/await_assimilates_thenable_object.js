// @fulfilled 9

async function run() {
  return await {
    then: function (resolve) {
      resolve(9);
    },
  };
}
