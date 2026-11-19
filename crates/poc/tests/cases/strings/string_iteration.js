// @fulfilled true

function collect(a, b, c, d, e, f) {
  return [a, b, c, d, e, f].join("|");
}

export async function run() {
  const text = "Aé汉😀e\u0301";
  const expected = "A|é|汉|😀|e|\u0301";
  let loop = [];
  for (const ch of text) {
    loop = [...loop, ch];
  }
  const symbol = Symbol.iterator;
  const iterator = text[symbol]();
  const other = text[symbol]();
  const empty = ""[symbol]().next();
  const next = iterator.next;
  let invalid = false;
  try {
    next();
  } catch (error) {
    invalid = error.name === "TypeError";
  }
  const first = iterator.next();
  const tail = [...iterator].join("|");
  const done = iterator.next();
  const again = iterator.next();
  return loop.join("|") === expected && [...text].join("|") === expected &&
    collect(...text) === expected && text[symbol] === ""[symbol] &&
    iterator[symbol]() === iterator && other.next().value === "A" &&
    first.value === "A" && !first.done && tail === "é|汉|😀|e|\u0301" &&
    empty.done && empty.value === undefined && done.done && done.value === undefined &&
    again.done && again.value === undefined && invalid;
}
