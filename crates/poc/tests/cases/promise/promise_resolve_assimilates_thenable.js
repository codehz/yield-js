// @fulfilled 7

export async function run() {
  return await Promise.resolve({
    then: function (resolve) {
      resolve(7);
    },
  });
}
