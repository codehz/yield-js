// @fulfilled "TypeError: boom | TypeError: boom"

export async function run() {
  if (new Error(42).message !== "42") return "wrong numeric message";
  const empty = TypeError();
  if (empty.name !== "TypeError" || empty.message !== "" || empty.hasOwnProperty("message")) {
    return "wrong default message";
  }
  if (Error("bare").stack.split("\n")[0] !== "Error: bare") return "wrong bare call";
  try {
    throw new TypeError("boom");
  } catch (e) {
    return e.name + ": " + e.message + " | " + e.stack.split("\n")[0];
  }
}
