// @fulfilled true
export function run() {
  return Promise.any([Promise.reject(1), Promise.reject(2)]).then(() => false, () => true);
}
