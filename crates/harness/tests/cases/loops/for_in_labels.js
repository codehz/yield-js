// @fulfilled "ab:2:b"

export async function run() {
  const target = {};
  let assignments = 0;
  let trace = "";
  function member() {
    assignments++;
    return "key";
  }

  outer: for (target[member()] in { a: true, b: true, c: true }) {
    do {
      trace += target.key;
      if (target.key === "a") continue outer;
      break outer;
    } while (false);
  }
  return trace + ":" + assignments + ":" + target.key;
}
