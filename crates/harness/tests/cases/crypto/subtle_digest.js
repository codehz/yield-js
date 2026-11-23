// @fulfilled "ok"

export async function run() {
  const bytes = new Uint8Array([0xff, 97, 98, 99, 0xff]);
  const order = [];
  const pending = crypto.subtle.digest({ name: "sha-256" }, bytes.subarray(1, 4));
  if (!(pending instanceof Promise)) throw "digest did not return a Promise";
  const result = pending.then(buffer => {
    order.push("digest");
    return buffer;
  });
  bytes.fill(0);
  order.push("sync");
  await Promise.resolve().then(() => order.push("microtask"));
  const digest = await result;
  if (order.join(",") !== "sync,microtask,digest") throw order.join(",");
  if (!(digest instanceof ArrayBuffer) || new Uint8Array(digest).toHex() !==
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad") {
    throw "SHA-256 digest or input window/copy mismatch";
  }
  return "ok";
}
