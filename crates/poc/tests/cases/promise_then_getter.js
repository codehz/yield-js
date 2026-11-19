// @fulfilled true

async function run() {
  let reads = 0;
  const thenable = {
    n: 7,
    accept(resolve) { resolve(this.n); },
    get then() { reads += 1; return this.accept; }
  };
  if ((await thenable) !== 7 || reads !== 1) return false;
  if ((await Promise.resolve(thenable)) !== 7 || reads !== 2) return false;
  if ((await Promise.resolve(5).finally(() => thenable)) !== 5 || reads !== 3) return false;

  let noncallableReads = 0;
  const noncallable = {get then() { noncallableReads += 1; return 0; }};
  if ((await Promise.resolve(noncallable)) !== noncallable || noncallableReads !== 1) return false;
  if ((await Promise.resolve(5).finally(() => noncallable)) !== 5 || noncallableReads !== 2) return false;

  let throws = 0;
  const bad = {get then() { throws += 1; throw "bad"; }};
  try { await bad; return false; } catch (e) { if (e !== "bad" || throws !== 1) return false; }
  const rejected = Promise.resolve(bad);
  try { await rejected; return false; } catch (e) { if (e !== "bad" || throws !== 2) return false; }
  try { await Promise.resolve(5).finally(() => bad); return false; }
  catch (e) { if (e !== "bad" || throws !== 3) return false; }
  try { await Promise.reject("original").finally(() => bad); return false; }
  catch (e) { return e === "bad" && throws === 4; }
}
