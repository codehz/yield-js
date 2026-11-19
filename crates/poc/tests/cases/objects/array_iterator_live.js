// @fulfilled true

async function run() {
  const a = [1, 2];
  const shared = a.values === a[Symbol.iterator] && !a.hasOwnProperty(Symbol.iterator);
  const it = a.values();
  const other = a[Symbol.iterator]();
  const iterable = it[Symbol.iterator]() === it;
  const first = it.next();
  a[1] = 7;
  a[2] = 8;
  const second = it.next();
  const third = it.next();
  const independent = other.next();
  const end = it.next();
  a[3] = 9;
  const after = it.next();
  const again = it.next();
  const empty = [].values().next();

  const next = it.next;
  let badNext = false;
  try {
    next();
  } catch (error) {
    badNext = error === "TypeError: ArrayIterator.next requires an array iterator";
  }
  const borrowed = { values: a.values, length: 1, 0: 42 };
  let badValues = false;
  try {
    borrowed.values();
  } catch (error) {
    badValues = error === "TypeError: Array.values requires an array";
  }

  it.next = 42;
  a[Symbol.iterator] = 43;
  return shared && iterable && first.value === 1 && first.done === false
    && second.value === 7 && second.done === false && third.value === 8 && third.done === false
    && independent.value === 1 && independent.done === false
    && end.done === true && after !== end && again !== after
    && after.done === true && again.done === true
    && after.hasOwnProperty("value") && after.value === undefined
    && again.hasOwnProperty("value") && again.value === undefined
    && empty.done === true && empty.hasOwnProperty("value") && empty.value === undefined
    && badNext && badValues && it.hasOwnProperty("next") && it.next === 42
    && a.hasOwnProperty(Symbol.iterator) && a[Symbol.iterator] === 43;
}
