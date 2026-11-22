// @returns "original:updated:7:getter,coerce,catch"

export function run() {
  let value = 'original';
  // Both operands must use the values read in evaluation order.
  const pair = value + (value = ':updated');
  let fromCatch = 0;
  const events = [];
  const object = {
    get field() { events.push('getter'); return 1; },
    valueOf() { events.push('coerce'); throw 7; },
  };
  try {
    42;
  } catch {
    return 'unexpected';
  }
  try {
    object.field;
    +object;
  } catch (error) {
    events.push('catch');
    fromCatch = error;
  }
  return pair + ':' + fromCatch + ':' + events.join(',');
}
