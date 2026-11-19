// @pending ["wait:a"]
// @fulfill ["wait:a", 1]
// @fulfilled 2

async function run() {
  let v = 1;
  const pending = new Promise(function (resolve) {
    resolve(2);
  });
  pending.then(function (value) {
    v = value;
  });
  if (v !== 1) {
    throw "not deferred";
  }
  await wait("a");
  return v;
}
