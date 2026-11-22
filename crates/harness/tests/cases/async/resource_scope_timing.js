// @fulfilled true

export async function run() {
  let boundary = "";
  async function onlyEmptyRecords() {
    using earlier = { [Symbol.dispose]() { boundary += "S"; } };
    await using absent1 = null, absent2 = undefined;
    Promise.resolve().then(() => { boundary += "1"; }).then(() => { boundary += "2"; });
  }
  await onlyEmptyRecords();
  if (boundary !== "1S2") return false;

  let trace = "";
  const marker = {};
  try {
    await using wrong = {
      get [Symbol.asyncDispose]() { trace += "G"; return false; },
      get [Symbol.dispose]() { trace += "wrong"; return () => {}; },
    };
    return false;
  } catch (e) {
    if (!(e instanceof TypeError) || trace !== "G") return false;
  }
  async function release() {
    using first = { [Symbol.dispose]() { trace += "S"; } };
    await using middle = {
      get [Symbol.asyncDispose]() { trace += "a"; return null; },
      get [Symbol.dispose]() { trace += "s"; return () => {
        trace += "F";
        return { get then() { trace += "wrong"; throw marker; } };
      }; },
    };
    await using absent1 = null, absent2 = undefined;
    await using last = { [Symbol.asyncDispose]() {
      trace += "T";
      return { get then() { trace += "g"; return resolve => { trace += "t"; resolve(); }; } };
    } };
    Promise.resolve().then(() => { trace += "1"; }).then(() => { trace += "2"; }).then(() => { trace += "3"; });
  }
  await release();
  if (trace !== "GasTg1t2F3S") return false;
  try {
    await using earlier = { [Symbol.dispose]() { trace += "e"; } };
    await using p = { [Symbol.asyncDispose]() {
      const result = Promise.resolve();
      Object.defineProperty(result, "constructor", { get() { trace += "c"; throw marker; } });
      return result;
    } };
  } catch (error) {
    return error === marker && trace === "GasTg1t2F3Sce";
  }
  return false;
}
