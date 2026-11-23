// @fulfilled "ok"
// Based on FileAPI Blob-constructor.html and blob-slice.html WPT assertions.

function eq(actual, expected, label) {
  if (actual !== expected) throw new Error(label + ": " + actual + " !== " + expected);
}

function typeError(action) {
  try { action(); } catch (error) { eq(error instanceof TypeError, true, "TypeError"); return; }
  throw new Error("expected TypeError");
}

export async function run() {
  eq(new Blob().size, 0, "empty Blob");
  typeError(() => Blob());
  typeError(() => new Blob("not a sequence"));
  typeError(() => new Blob([], { endings: "invalid" }));
  eq(new Blob([], { type: "bad\x00type" }).type, "", "invalid MIME");
  const source = new Uint8Array([33, 65, 66, 33]);
  const window = new DataView(source.buffer, 1, 2);
  const blob = new Blob(new Set([window, new Blob(["C"]), "\ud800"]), { type: "TEXT/PLAIN" });
  source.fill(0);
  eq(blob.size, 6, "size in bytes");
  eq(blob.type, "text/plain", "normalized MIME");
  eq(await blob.text(), "ABC\ufffd", "copied window and USV string");
  const sliced = blob.slice(-5, 3, "X/Y");
  eq(sliced.type, "x/y", "slice MIME");
  eq(await sliced.text(), "BC", "negative offset slice");
  eq(await blob.slice(0.5, 2.5).text(), "AB", "clamp rounds ties to even");
  eq(blob.slice(3, 1).size, 0, "reversed bounds");
  const bytes = await sliced.bytes();
  eq(bytes instanceof Uint8Array, true, "bytes result");
  bytes[0] = 0;
  const buffer = await sliced.arrayBuffer();
  eq(new Uint8Array(buffer).join(","), "66,67", "independent byte outputs");
  eq(await new Blob([new Uint8Array([239, 187, 191, 120, 255])]).text(), "x\ufffd", "UTF-8 BOM and malformed bytes");

  const reader = sliced.stream().getReader({ mode: "byob" });
  const first = await reader.read(new Uint8Array(1));
  eq(first.value[0], 66, "BYOB first byte");
  const last = await reader.read(new Uint8Array(2));
  eq(last.value.join(","), "67", "BYOB tail");
  eq((await reader.read(new Uint8Array(1))).done, true, "finished byte stream");
  eq((await new Blob().stream().getReader().read()).done, true, "empty byte stream");

  const fake = Object.create(Blob.prototype);
  typeError(() => fake.size);
  typeError(() => fake.slice());
  typeError(() => fake.stream());
  for (const method of ["bytes", "text", "arrayBuffer"]) {
    let rejected = false;
    const result = fake[method]();
    try { await result; } catch (error) { rejected = error instanceof TypeError; }
    eq(rejected, true, method + " brand rejection");
  }
  const cloned = structuredClone(sliced);
  eq(cloned instanceof Blob, true, "clone brand");
  eq(await cloned.text(), "BC", "clone shares immutable bytes");
  eq(await sliced.text(), "BC", "source still readable");
  return "ok";
}
