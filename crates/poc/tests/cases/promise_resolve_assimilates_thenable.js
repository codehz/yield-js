// @fulfilled 7

async function run() {
  return await Promise.resolve({
    then: function (resolve) {
      resolve(7);
    },
  });
}
