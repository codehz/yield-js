// @fulfilled "ok"

export async function run() {
  const subtle = crypto.subtle;
  // RFC 4231, test case 1.
  const key = await subtle.importKey("raw", new Uint8Array(20).fill(0x0b),
    { name: "HMAC", hash: { name: "SHA-256" } }, true, ["sign", "verify"]);
  const message = new Uint8Array([72, 105, 32, 84, 104, 101, 114, 101]); // Hi There
  const signature = new Uint8Array(await subtle.sign("HMAC", key, message));
  if (signature.toHex() !== "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7") {
    throw "RFC 4231 HMAC mismatch";
  }
  if (await subtle.verify("HMAC", key, signature, message) !== true) throw "valid signature rejected";
  if (await subtle.verify("HMAC", key, signature.subarray(0, 31), message) !== false) {
    throw "short signature verified";
  }
  signature[31] ^= 1;
  if (await subtle.verify("HMAC", key, signature, message) !== false) throw "bad signature verified";
  return "ok";
}
