// @fulfilled true

export async function run() {
  let trace = "";
  let i = 0;
  for (using first = { [Symbol.dispose]() { trace += "first/"; } },
              second = { [Symbol.dispose]() { trace += "second/"; } };
       i < 2; i++) {
    trace += `body${i}/`;
    if (!i) continue;
  }
  if (trace !== "body0/body1/second/first/") return false;
  trace = "";
  i = 0;
  loop: for (await using only = { async [Symbol.asyncDispose]() {
    await undefined;
    trace += "dispose/";
  } }; i < 3; i++) {
    trace += `body${i}/`;
    if (!i) continue loop;
    break loop;
  }
  return trace === "body0/body1/dispose/";
}
