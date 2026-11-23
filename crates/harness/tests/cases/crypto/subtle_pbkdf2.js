// @fulfilled "ok"

export async function run() {
  const subtle = crypto.subtle;
  const password = await subtle.importKey("raw", new Uint8Array([112, 97, 115, 115, 119, 111, 114, 100]),
    "PBKDF2", true, ["deriveKey"]);
  const derived = await subtle.deriveKey({ name: "PBKDF2", salt: new Uint8Array([115, 97, 108, 116]),
    iterations: 2, hash: "SHA-256" }, password, { name: "AES-GCM", length: 256 }, true, ["encrypt", "decrypt"]);
  if (new Uint8Array(await subtle.exportKey("raw", derived)).toHex() !==
      "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43") {
    throw "PBKDF2-HMAC-SHA256 mismatch";
  }
  const params = { name: "AES-GCM", iv: new Uint8Array(12) };
  const clear = new Uint8Array([1, 2, 3]);
  const sealed = await subtle.encrypt(params, derived, clear);
  if (new Uint8Array(await subtle.decrypt(params, derived, sealed)).toHex() !== clear.toHex()) {
    throw "derived AES key roundtrip mismatch";
  }
  return "ok";
}
