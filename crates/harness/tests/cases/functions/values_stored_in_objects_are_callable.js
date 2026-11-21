// @returns 5

export function run() {
  const obj = {
    f: function(x) {
      return x + 1;
    }
  };
  return obj.f(4);
}
