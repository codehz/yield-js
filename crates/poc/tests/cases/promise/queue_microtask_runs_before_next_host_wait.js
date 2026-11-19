// @pending ["wait:a"]
// @fulfill ["wait:a", 0]
// @fulfilled 1

async function run() {
  let v = 0;
  queueMicrotask(function () {
    v = 1;
  });
  if (v !== 0) {
    throw "not deferred";
  }
  await wait("a");
  return v;
}
