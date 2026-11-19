// @fulfilled true

async function run() {
  let x = 0;
  let skipped = (x &&= await wait("skip"));
  if (skipped !== 0 || x !== 0) {
    return false;
  }
  let still = (x ??= 9);
  if (still !== 0 || x !== 0) {
    return false;
  }
  let assigned = (x ||= 4);
  if (assigned !== 4 || x !== 4) {
    return false;
  }
  let kept = (x ||= await wait("skip"));
  if (kept !== 4 || x !== 4) {
    return false;
  }
  let updated = (x &&= 5);
  if (updated !== 5 || x !== 5) {
    return false;
  }

  x = null;
  let fromNull = (x ??= 6);
  if (fromNull !== 6 || x !== 6) {
    return false;
  }
  let missing;
  x = missing;
  let fromMissing = (x ??= 6);
  return fromMissing === 6 && x === 6;
}
