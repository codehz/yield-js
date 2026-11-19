// @fulfilled true

export async function run() {
  const p = Promise.resolve(1);
  const reject = () => 0;
  let reads = 0;
  Object.defineProperty(p, "then", {
    get() {
      reads++;
      return function (ok, bad) {
        return this === p && ok === undefined && bad === reject && arguments.length === 2;
      };
    },
  });
  return p.catch(reject) && reads === 1;
}
