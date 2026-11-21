// @returns "ok"

const encoder = new TextEncoder();
const decoder = new TextDecoder();

function eq(actual, expected, label) {
  if (actual !== expected) {
    throw label + ": expected " + JSON.stringify(expected) + ", got " + JSON.stringify(actual);
  }
}

function join(bytes) {
  return Array.from(bytes).join(",");
}

function throwsError(kind, fn, label) {
  try {
    fn();
  } catch (error) {
    if (!(error instanceof kind)) {
      throw label + ": wrong error type";
    }
    return error;
  }
  throw label + ": expected a throw";
}

export function run() {
  eq(encoder.encoding, "utf-8", "encoder encoding");
  eq(decoder.encoding, "utf-8", "decoder encoding");
  eq(decoder.fatal, false, "fatal default");
  eq(decoder.ignoreBOM, false, "ignoreBOM default");
  eq(TextEncoder.length, 0, "encoder constructor length");
  eq(TextDecoder.length, 0, "decoder constructor length");
  eq(TextEncoder.prototype.encode.length, 0, "encode length");
  eq(TextEncoder.prototype.encodeInto.length, 2, "encodeInto length");
  eq(TextDecoder.prototype.decode.length, 0, "decode length");
  const fatalOptions = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });
  eq(fatalOptions.fatal, true, "fatal option");
  eq(fatalOptions.ignoreBOM, true, "ignoreBOM option");
  const optionFailure = {};
  eq(
    throwsError(
      Object,
      () =>
        new TextDecoder("utf-8", {
          get fatal() {
            throw optionFailure;
          },
        }),
      "fatal getter",
    ),
    optionFailure,
    "fatal getter identity",
  );
  eq(Object.prototype.toString.call(encoder), "[object TextEncoder]", "encoder tag");
  eq(Object.prototype.toString.call(decoder), "[object TextDecoder]", "decoder tag");
  eq(new TextDecoder(" UTF8 ").encoding, "utf-8", "label trimming");
  throwsError(RangeError, () => new TextDecoder("gbk"), "unsupported label");
  throwsError(RangeError, () => new TextDecoder(""), "empty label");
  throwsError(TypeError, () => TextEncoder(), "encoder without new");
  throwsError(TypeError, () => TextDecoder(), "decoder without new");
  throwsError(
    TypeError,
    () => new TextDecoder("utf-8", 1),
    "non-object options",
  );

  eq(
    join(encoder.encode("A😀\uD800")),
    "65,240,159,152,128,239,191,189",
    "encode replaces a lone surrogate",
  );
  eq(join(encoder.encode()), "", "encode without an argument is empty");
  eq(encoder.encode().length, 0, "empty encode");

  const backing = new Uint8Array(8).fill(9);
  const narrow = new Uint8Array(backing.buffer, 2, 4);
  const narrowResult = encoder.encodeInto("A😀", narrow);
  eq(narrowResult.read, 1, "narrow read");
  eq(narrowResult.written, 1, "narrow written");
  eq(join(backing), "9,9,65,9,9,9,9,9", "narrow writes no partial scalar");

  const widerBacking = new Uint8Array(7).fill(9);
  const wider = new Uint8Array(widerBacking.buffer, 1, 5);
  const widerResult = encoder.encodeInto("A😀", wider);
  eq(widerResult.read, 3, "wide read");
  eq(widerResult.written, 5, "wide written");
  eq(join(widerBacking), "9,65,240,159,152,128,9", "wide bytes");

  throwsError(
    TypeError,
    () => encoder.encodeInto("A", [0]),
    "plain array destination",
  );
  throwsError(
    TypeError,
    () => encoder.encodeInto("A", new Uint8ClampedArray(1)),
    "clamped destination",
  );
  throwsError(
    TypeError,
    () => encoder.encodeInto("A"),
    "missing destination",
  );

  const euro = new TextDecoder();
  const emoji = new TextDecoder();
  eq(euro.decode(Uint8Array.from([0xe2]), { stream: true }), "", "euro first chunk");
  eq(emoji.decode(Uint8Array.from([0xf0, 0x9f]), { stream: true }), "", "emoji first chunk");
  eq(euro.decode(Uint8Array.from([0x82, 0xac])), "€", "euro second chunk");
  eq(emoji.decode(Uint8Array.from([0x98, 0x80])), "😀", "emoji second chunk");

  eq(new TextDecoder().decode(Uint8Array.from([0xe2, 0x28, 0xa1])), "�(�", "invalid sequence");

  const split = new TextDecoder();
  eq(split.decode(Uint8Array.from([0xe2, 0x82]), { stream: true }), "", "split tail");
  eq(split.decode(), "�", "split flush");
  eq(split.decode(), "", "repeated flush");

  const fatal = new TextDecoder("utf-8", { fatal: true });
  eq(fatal.decode(Uint8Array.from([0xe2, 0x82]), { stream: true }), "", "fatal tail");
  throwsError(TypeError, () => fatal.decode(), "fatal flush");
  eq(fatal.decode(Uint8Array.from([0x41])), "A", "fatal flush restarts the stream");

  const retained = new TextDecoder("utf-8", { fatal: true });
  throwsError(
    TypeError,
    () => retained.decode(Uint8Array.from([0x41, 0xff, 0x62]), { stream: true }),
    "fatal stream error",
  );
  eq(retained.decode(), "b", "fatal stream retains the unprocessed queue");

  const buffer = new ArrayBuffer(3);
  new Uint8Array(buffer).set([0x41, 0x42, 0x43]);
  eq(new TextDecoder().decode(buffer), "ABC", "ArrayBuffer source");

  const rawBuffer = new Uint8Array([0xc3, 0xa9, 0xf0, 0x9f, 0x98, 0x80]).buffer;
  eq(
    new TextDecoder().decode(new Uint16Array(rawBuffer)),
    "é😀",
    "non-byte view reads the raw window",
  );

  const bom = new TextDecoder();
  eq(bom.decode(Uint8Array.from([0xef, 0xbb, 0xbf, 0x41])), "A", "BOM stripped");
  eq(bom.decode(Uint8Array.from([0xef, 0xbb, 0xbf, 0x41])), "A", "new stream strips BOM");
  const keepBom = new TextDecoder("utf-8", { ignoreBOM: true });
  eq(keepBom.decode(Uint8Array.from([0xef, 0xbb, 0xbf, 0x41])), "\uFEFFA", "BOM kept");

  const emptyChunk = new TextDecoder();
  eq(emptyChunk.decode(new Uint8Array(0), { stream: true }), "", "empty stream chunk");
  eq(
    emptyChunk.decode(Uint8Array.from([0xef, 0xbb, 0xbf, 0x41])),
    "A",
    "empty chunk does not consume BOM handling",
  );

  const detached = new ArrayBuffer(2);
  detached.transfer();
  eq(new TextDecoder().decode(detached), "", "detached buffer is empty");

  const detachedDestination = new Uint8Array(2);
  detachedDestination.buffer.transfer();
  const detachedResult = encoder.encodeInto("A", detachedDestination);
  eq(detachedResult.read, 0, "detached destination read");
  eq(detachedResult.written, 0, "detached destination written");

  const shrinkable = new ArrayBuffer(4, { maxByteLength: 4 });
  const stale = new Uint8Array(shrinkable);
  shrinkable.resize(0);
  eq(new TextDecoder().decode(stale), "", "out-of-bounds view is empty");

  const throwing = new TextDecoder();
  eq(throwing.decode(Uint8Array.from([0xe2, 0x82]), { stream: true }), "", "throw first");
  const failure = {};
  eq(
    throwsError(
      Object,
      () =>
        throwing.decode(undefined, {
          get stream() {
            throw failure;
          },
        }),
      "stream getter",
    ),
    failure,
    "stream getter identity",
  );
  eq(throwing.decode(), "�", "stream getter keeps the prefix");

  const fake = Object.create(TextDecoder.prototype);
  throwsError(TypeError, () => fake.decode(), "forged decoder");
  throwsError(TypeError, () => TextDecoder.prototype.decode.call({}), "borrowed decode");
  const fakeEncoder = Object.create(TextEncoder.prototype);
  throwsError(TypeError, () => fakeEncoder.encode("A"), "forged encoder");

  class MyDecoder extends TextDecoder {}
  const mine = new MyDecoder();
  eq(Object.getPrototypeOf(mine), MyDecoder.prototype, "newTarget prototype");
  eq(mine.encoding, "utf-8", "subclass decoding");

  return "ok";
}
