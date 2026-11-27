// @advance 1
// @fulfilled true

import { assert, gc } from "test";

export async function run() {
  function makeRef() {
    return new WeakRef({});
  }
  const ref = makeRef();
  gc();
  assert(ref.deref() !== undefined);
  let microHit = false;
  queueMicrotask(() => {
    gc();
    microHit = ref.deref() !== undefined;
  });
  await Promise.resolve();
  assert(microHit);

  let laterUndefined = false;
  setTimeout(() => {
    gc();
    laterUndefined = ref.deref() === undefined;
  }, 0);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert(laterUndefined);

  const a = {};
  const keep = [];
  const dual = new FinalizationRegistry(() => {});
  (function pair() {
    const first = {};
    const second = {};
    dual.register(first, second);
    dual.register(second, "second");
    keep.push(new WeakRef(second));
  })();
  gc();
  assert(keep[0].deref() !== undefined);
  const delivered = [];
  const token = {};
  const other = {};
  const throwing = new FinalizationRegistry((held) => {
    delivered.push(held);
    if (held === "unreg") {
      throwing.unregister(token);
    }
    if (held === "gc") {
      gc();
    }
    if (held === "throw") {
      throw "boom";
    }
  });
  function registerTemps() {
    throwing.register({}, "first");
    throwing.register({}, "unreg", token);
    throwing.register(other, "kept", token);
    throwing.register({}, "gc");
    throwing.register({}, "throw");
    throwing.register({}, "after");
  }
  registerTemps();
  gc();
  assert(delivered.length === 0);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert(delivered[0] === "first");
  assert(delivered.includes("unreg"));
  assert(delivered.includes("gc"));
  assert(delivered.includes("throw"));
  assert(delivered.includes("after"));
  assert(!delivered.includes("kept"));
  assert(delivered.filter((item) => item === "first").length === 1);
  return true;
}
