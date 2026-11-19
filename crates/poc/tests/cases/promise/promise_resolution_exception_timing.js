// @fulfilled "static;catch:5;finally;returned;then:get;then:get;then:catch;then:catch;"

export async function run() {
  let trace = "";
  const reason = {};
  const source = Promise.resolve(1);
  Object.defineProperty(source, "constructor", { get() { throw reason; } });
  Object.defineProperty(source, "then", { get() { throw "wrong getter"; } });
  try {
    Promise.resolve(source);
    throw "must throw synchronously";
  } catch (error) {
    if (error !== reason) throw error;
    trace += "static;";
  }
  async function consume(input) {
    const live = input + 1;
    try {
      await source;
    } catch (error) {
      if (error !== reason) throw error;
      trace += "catch:" + live + ";";
    } finally {
      trace += "finally;";
    }
  }
  const result = consume(4);
  trace += "returned;";
  await result;
  const mismatch = Promise.resolve(1);
  mismatch.constructor = null;
  Object.defineProperty(mismatch, "then", { get() { trace += "then:get;"; throw reason; } });
  const rejected = Promise.resolve(mismatch);
  const consumeMismatch = async () => {
    try { await mismatch; } catch (error) {
      if (error !== reason) throw error;
      trace += "then:catch;";
    }
  };
  const mismatchResult = consumeMismatch();
  if (trace !== "static;catch:5;finally;returned;then:get;then:get;") throw trace;
  try {
    await rejected;
    throw "must reject";
  } catch (error) {
    if (error !== reason) throw error;
    trace += "then:catch;";
  }
  await mismatchResult;
  return trace;
}
