// @fulfilled "ok"

async function expectAuthFailure(promise) {
  try {
    await promise;
  } catch (e) {
    if (e instanceof DOMException && e.name === "OperationError") return;
    throw e;
  }
  throw "unauthenticated AES-GCM input was accepted";
}

export async function run() {
  const subtle = crypto.subtle;
  const hex = Uint8Array.fromHex;
  // NIST AES-GCM example with a 96-bit IV and nonempty AAD.
  const key = await subtle.importKey("raw", hex("feffe9928665731c6d6a8f9467308308"),
    "AES-GCM", true, ["encrypt", "decrypt"]);
  const iv = hex("cafebabefacedbaddecaf888");
  const additionalData = hex("feedfacedeadbeeffeedfacedeadbeefabaddad2");
  const plaintext = "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a72" +
    "1c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39";
  const ciphertext = "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e" +
    "21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091";
  const tag = "5bc94fbc3221a5db94fae95ae7121a47";
  for (const tagLength of [128, 96]) {
    const params = { name: "AES-GCM", iv, additionalData, tagLength };
    const expected = ciphertext + tag.slice(0, tagLength / 4);
    const sealed = new Uint8Array(await subtle.encrypt(params, key, hex(plaintext)));
    if (sealed.toHex() !== expected) throw "NIST AES-GCM encryption mismatch: " + tagLength;
    const opened = await subtle.decrypt(params, key, hex(expected));
    if (new Uint8Array(opened).toHex() !== plaintext) throw "NIST AES-GCM decryption mismatch: " + tagLength;
    const alteredAAD = additionalData.slice();
    alteredAAD[0] ^= 1;
    await expectAuthFailure(subtle.decrypt({ name: "AES-GCM", iv, additionalData: alteredAAD, tagLength }, key, sealed));
    sealed[sealed.length - 1] ^= 1;
    await expectAuthFailure(subtle.decrypt(params, key, sealed));
  }
  return "ok";
}
