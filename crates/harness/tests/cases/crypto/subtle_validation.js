// @fulfilled "ok"

async function rejects(kind, call) {
  const promise = call(); // A synchronous throw is not a valid rejection.
  if (!(promise instanceof Promise)) throw "expected a native Promise";
  try {
    await promise;
  } catch (e) {
    if (kind === "TypeError" ? e instanceof TypeError : e instanceof DOMException && e.name === kind) return;
    throw e;
  }
  throw "expected rejection: " + kind;
}

function throwsTypeError(call) {
  try {
    call();
  } catch (e) {
    if (e instanceof TypeError) return;
    throw e;
  }
  throw "expected synchronous TypeError";
}

export async function run() {
  const subtle = crypto.subtle;
  if (subtle !== crypto.subtle || !(subtle instanceof SubtleCrypto) ||
      Object.prototype.toString.call(subtle) !== "[object SubtleCrypto]" ||
      Object.getOwnPropertyDescriptor(Object.getPrototypeOf(crypto), "subtle").set !== undefined) {
    throw "SubtleCrypto singleton or readonly metadata mismatch";
  }
  throwsTypeError(() => new SubtleCrypto());
  throwsTypeError(() => new CryptoKey());

  const raw = new Uint8Array(18).fill(0xff);
  raw[0] = 0x11;
  raw[17] = 0x22;
  const view = new DataView(raw.buffer, 1, 16);
  const algorithm = { name: "aes-gcm" };
  const usages = ["encrypt"];
  const imported = subtle.importKey("raw", view, algorithm, true, usages);
  if (!(imported instanceof Promise)) throw "importKey did not return a native Promise";
  let settled = false;
  imported.then(() => { settled = true; });
  raw.fill(0);
  algorithm.name = "HMAC";
  usages[0] = "decrypt";
  if (settled) throw "importKey settled synchronously";
  const key = await imported;
  if (!settled || !(key instanceof CryptoKey) || Object.prototype.toString.call(key) !== "[object CryptoKey]" ||
      key.type !== "secret" || key.extractable !== true || key.algorithm.name !== "AES-GCM" ||
      key.algorithm.length !== 128 || key.usages.join(",") !== "encrypt") throw "CryptoKey metadata mismatch";
  key.usages.push("decrypt");
  key.algorithm.name = "HMAC";
  if (key.usages.join(",") !== "encrypt" || key.algorithm.name !== "AES-GCM") throw "metadata was aliased";
  const exported = new Uint8Array(await subtle.exportKey("raw", key));
  if (exported.toHex() !== "ffffffffffffffffffffffffffffffff") throw "import window/copy mismatch";
  exported.fill(0);
  if (new Uint8Array(await subtle.exportKey("raw", key)).toHex() !== "ffffffffffffffffffffffffffffffff") {
    throw "export exposed key storage";
  }

  const fake = Object.create(CryptoKey.prototype);
  for (const field of ["type", "extractable", "algorithm", "usages"]) {
    const descriptor = Object.getOwnPropertyDescriptor(CryptoKey.prototype, field);
    if (descriptor.set !== undefined) throw "writable CryptoKey metadata: " + field;
    throwsTypeError(() => descriptor.get.call(fake));
  }
  await rejects("TypeError", () => subtle.exportKey("raw", fake));
  await rejects("TypeError", () => subtle.exportKey("raw", new Proxy(fake, {})));
  await rejects("TypeError", () => subtle.digest.call(Object.create(SubtleCrypto.prototype), "SHA-256", raw));
  await rejects("NotSupportedError", () => subtle.digest("SHA-1", raw));
  await rejects("NotSupportedError", () => subtle.importKey("jwk", view, "AES-GCM", true, ["encrypt"]));
  await rejects("NotSupportedError", () => subtle.exportKey("spki", key));
  await rejects("NotSupportedError", () => subtle.importKey("raw", view, "AES-CBC", true, ["encrypt"]));
  await rejects("NotSupportedError", () => subtle.importKey("raw", view, { name: "HMAC", hash: "SHA-1" }, true, ["sign"]));
  await rejects("NotSupportedError", () => subtle.importKey("raw", view, "AES-GCM", false, ["encrypt"]));
  await rejects("SyntaxError", () => subtle.importKey("raw", view, "AES-GCM", true, ["sign"]));
  await rejects("SyntaxError", () => subtle.importKey("raw", view, "AES-GCM", true, []));
  await rejects("DataError", () => subtle.importKey("raw", raw, "AES-GCM", true, ["encrypt"]));
  await rejects("DataError", () => subtle.importKey("raw", view, { name: "HMAC", hash: "SHA-256", length: 256 }, true, ["sign"]));
  await rejects("InvalidAccessError", () => subtle.sign("HMAC", key, raw));
  const params = { name: "AES-GCM", iv: new Uint8Array(12) };
  await rejects("InvalidAccessError", () => subtle.decrypt(params, key, raw));
  await rejects("OperationError", () => subtle.encrypt({ name: "AES-GCM", iv: params.iv, tagLength: 95 }, key, raw));

  const password = await subtle.importKey("raw", view, "PBKDF2", true, ["deriveKey"]);
  const pbkdf2 = { name: "PBKDF2", hash: "SHA-256", salt: raw, iterations: 1 };
  await rejects("OperationError", () => subtle.deriveKey(pbkdf2, password, { name: "AES-GCM", length: 64 }, true, ["encrypt"]));
  await rejects("NotSupportedError", () => subtle.deriveKey(pbkdf2, password, { name: "AES-GCM", length: 128 }, false, ["encrypt"]));

  const detached = new Uint8Array(4);
  detached.buffer.transfer();
  const resizable = new ArrayBuffer(8, { maxByteLength: 16 });
  const outOfBounds = new DataView(resizable, 4, 4);
  resizable.resize(4);
  for (const invalid of [[], new Proxy(raw, {}), detached, outOfBounds]) {
    await rejects("TypeError", () => subtle.digest("SHA-256", invalid));
  }
  return "ok";
}
