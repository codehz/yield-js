// @fulfilled "0:1:2|0:1:2|0:1:2"

export async function run() {
  async function* forLoop(i) {
    ;
    for (;;) {
      yield i++;
      if (i === 2) break;
      continue;
    }
    return i;
  }
  async function* whileLoop(i) {
    while (i < 2) {
      yield i++;
    }
    return i;
  }
  async function* doWhileLoop(i) {
    do {
      yield i++;
    } while (i < 2);
    return i;
  }

  const results = [];
  for (const g of [forLoop, whileLoop, doWhileLoop]) {
    const it = g(0);
    const first = await it.next();
    const second = await it.next();
    const last = await it.next();
    if (first.done || second.done || !last.done) return "unexpected completion";
    results.push([first.value, second.value, last.value].join(":"));
  }
  return results.join("|");
}
