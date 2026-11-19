// @fulfilled "tick;adopted;"

export async function run() {
  let trace = "";
  const source = Promise.resolve(1);
  const outer = new Promise(resolve => resolve(source));
  outer.then(() => { trace += "adopted;"; });
  queueMicrotask(() => queueMicrotask(() => { trace += "tick;"; }));
  await outer;
  return trace;
}
