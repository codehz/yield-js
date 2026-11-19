// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 1]
// @fulfilled "late"

async function run() {
  return await Promise.resolve("late").finally(function () {
    return wait("a");
  });
}
