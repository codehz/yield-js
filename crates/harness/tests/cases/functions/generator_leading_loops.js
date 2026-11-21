// @returns "0:1:2|0:1:2|0:1:2"

export function run() {
  function* forLoop(i) {
    ;
    for (;;) {
      yield i++;
      if (i === 2) break;
      continue;
    }
    return i;
  }
  function* whileLoop(i) {
    while (i < 2) {
      yield i++;
    }
    return i;
  }
  function* doWhileLoop(i) {
    do {
      yield i++;
    } while (i < 2);
    return i;
  }

  const results = [];
  for (const g of [forLoop, whileLoop, doWhileLoop]) {
    const it = g(0);
    const first = it.next();
    const second = it.next();
    const last = it.next();
    if (first.done || second.done || !last.done) return "unexpected completion";
    results.push([first.value, second.value, last.value].join(":"));
  }
  return results.join("|");
}
