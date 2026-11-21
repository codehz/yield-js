// @returns "ok"

let where = "";

function eq(actual, expected, label) {
  if (actual !== expected) {
    throw label + ": expected " + expected + ", got " + actual;
  }
}

function throwsError(kind, fn, label) {
  try {
    fn();
  } catch (error) {
    if (!(error instanceof kind)) {
      throw label + ": wrong error type";
    }
    return;
  }
  throw label + ": expected a throw";
}

function join(view) {
  return Array.from(view).join(",");
}

export function run() {
  where = "fromBase64";
  eq(join(Uint8Array.fromBase64("AP8=")), "0,255", "padded");
  eq(join(Uint8Array.fromBase64("  AP8= \n")), "0,255", "whitespace");
  eq(join(Uint8Array.fromBase64("AP8")), "0,255", "unpadded");
  eq(join(Uint8Array.fromBase64("AP")), "0", "one byte");
  eq(join(Uint8Array.fromBase64("")), "", "empty");
  eq(
    join(Uint8Array.fromBase64("_-8=", { alphabet: "base64url" })),
    "255,239",
    "url alphabet",
  );
  throwsError(
    SyntaxError,
    () => Uint8Array.fromBase64("_-8="),
    "url characters in the standard alphabet",
  );
  throwsError(
    SyntaxError,
    () => Uint8Array.fromBase64("A"),
    "single trailing character",
  );
  throwsError(
    SyntaxError,
    () => Uint8Array.fromBase64("A*8="),
    "non-alphabet character",
  );
  throwsError(
    SyntaxError,
    () => Uint8Array.fromBase64("A=8="),
    "short chunk before padding",
  );
  throwsError(
    SyntaxError,
    () => Uint8Array.fromBase64("AP8=A"),
    "data after padding",
  );
  throwsError(
    SyntaxError,
    () => Uint8Array.fromBase64("AP8", { lastChunkHandling: "strict" }),
    "strict rejects a partial chunk",
  );
  eq(
    join(Uint8Array.fromBase64("AP8=", { lastChunkHandling: "strict" })),
    "0,255",
    "strict with padding",
  );
  eq(
    join(Uint8Array.fromBase64("AP8", { lastChunkHandling: "stop-before-partial" })),
    "",
    "stop before partial",
  );
  eq(
    join(Uint8Array.fromBase64("AP8=", { lastChunkHandling: "stop-before-partial" })),
    "0,255",
    "stop before partial keeps a padded chunk",
  );
  eq(
    join(Uint8Array.fromBase64("AAAAAP8=")),
    "0,0,0,0,255",
    "several chunks",
  );

  where = "toBase64";
  eq(Uint8Array.from([0, 255]).toBase64(), "AP8=", "round trip");
  eq(
    Uint8Array.from([251, 255]).toBase64({ alphabet: "base64url" }),
    "-_8=",
    "url output",
  );
  eq(Uint8Array.from([0]).toBase64({ omitPadding: true }), "AA", "omit padding");
  eq(new Uint8Array(0).toBase64(), "", "empty output");

  where = "setFromBase64";
  let target = new Uint8Array(4);
  let result = target.setFromBase64("AP8=");
  eq(result.read, 4, "read");
  eq(result.written, 2, "written");
  eq(join(target), "0,255,0,0", "written bytes");
  eq(
    JSON.stringify(new Uint8Array(1).setFromBase64("AP8=")),
    '{"read":0,"written":0}',
    "too small a target",
  );
  eq(
    JSON.stringify(new Uint8Array(2).setFromBase64("AP8=")),
    '{"read":4,"written":2}',
    "exact target",
  );
  const partialTarget = new Uint8Array(4);
  const partialResult = partialTarget.setFromBase64("AAAAAP8=");
  eq(partialResult.read, 4, "partial chunk read");
  eq(partialResult.written, 3, "partial chunk written");
  eq(join(partialTarget), "0,0,0,0", "partial chunk bytes");
  const fullTarget = new Uint8Array(5);
  const fullResult = fullTarget.setFromBase64("AAAAAP8=");
  eq(fullResult.read, 8, "full target read");
  eq(fullResult.written, 5, "full target written");
  eq(join(fullTarget), "0,0,0,0,255", "full target bytes");

  where = "hex";
  eq(join(Uint8Array.fromHex("00ffAb")), "0,255,171", "mixed case");
  eq(Uint8Array.fromHex("").length, 0, "empty hex");
  throwsError(SyntaxError, () => Uint8Array.fromHex("0g"), "non-hex digit");
  throwsError(SyntaxError, () => Uint8Array.fromHex("abc"), "odd length");
  eq(Uint8Array.from([0, 255, 171]).toHex(), "00ffab", "toHex");
  const hexTarget = new Uint8Array(1);
  const hexResult = hexTarget.setFromHex("00ff");
  eq(hexResult.read, 2, "hex read");
  eq(hexResult.written, 1, "hex written");
  eq(hexTarget[0], 0, "hex byte");

  where = "detached";
  const buffer = new ArrayBuffer(2);
  const view = new Uint8Array(buffer);
  buffer.transfer();
  throwsError(TypeError, () => view.toBase64(), "detached toBase64");
  throwsError(TypeError, () => view.toHex(), "detached toHex");
  throwsError(TypeError, () => view.setFromHex("00"), "detached setFromHex");
  return "ok";
}
