// @returns 4

export function run() {
  const obj = {
    value: 4,
    method: function () {
      return this.value;
    },
  };
  return obj.method();
}
