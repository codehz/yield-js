// @fulfilled "3i4imcao7TFV"

function stream(marker, record) {
  return {
    [Symbol.iterator]() { return this; },
    next() { return { value: 1 }; },
    return() { record(marker); return {}; }
  };
}

function earlyReturn(record) {
  for (const ignored of stream("o", record)) return 7;
}

async function run() {
  let trace = "";
  const record = value => { trace += value; };
  let item;
  outer: for (item of [3, 4]) {
    record(item);
    for (const ignored of stream("i", record)) {
      try { continue outer; }
      finally { }
    }
  }
  const target = { set x(value) { record("m"); throw "a"; } };
  try {
    for (target.x of stream("c", record)) { }
  } catch (error) { record(error); }
  record(earlyReturn(record));
  const failing = stream("", record);
  failing.return = () => 1;
  try {
    for (const x of failing) break;
  } catch (error) { if (error.name === "TypeError") record("T"); }
  const broken = stream("wrong close", record);
  broken.next = () => ({ get value() { throw "F"; } });
  try {
    for (const x of broken) { }
  } catch (error) { record(error); }
  broken.next = () => ({ done: true, get value() { record("wrong value"); } });
  for (const x of broken) { }
  const bad = stream("wrong close", record);
  bad.next = () => 0;
  try {
    for (const x of bad) { }
  } catch (error) { if (error.name === "TypeError") record("V"); }
  return trace;
}
