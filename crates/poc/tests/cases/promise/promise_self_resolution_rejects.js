// @fulfilled true

export async function run() {
  let finish;
  const direct = new Promise((resolve, reject) => {
    finish = () => { resolve(direct); reject("ignored"); };
  });
  let reads = 0;
  Object.defineProperty(direct, "then", {get() { reads += 1; throw "unexpected getter"; }});
  finish();
  try { await direct; return false; }
  catch (error) { if (error.name !== "TypeError" || reads !== 0) return false; }

  const chain = Promise.resolve(0).then(() => chain);
  try { await chain; return false; }
  catch (error) { if (error.name !== "TypeError") return false; }

  async function returnSelf() { await 0; return pending; }
  const pending = returnSelf();
  try { await pending; return false; }
  catch (error) { if (error.name !== "TypeError") return false; }

  async function returnThenable() { return {then(resolve) { resolve(42); }}; }
  return (await returnThenable()) === 42;
}
