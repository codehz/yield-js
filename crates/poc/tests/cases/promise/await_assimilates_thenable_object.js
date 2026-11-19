// @fulfilled 9

export async function run() {
  return await {
    then: function (resolve) {
      resolve(9);
    },
  };
}
