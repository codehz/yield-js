// @fulfilled "ok"
// Offline Fetch schemes. Assertions follow the Fetch Standard data: and blob:
// URL processing rules; HTTP without a host adapter must reject.

function eq(actual, expected, label) {
  if (actual !== expected) throw new Error(label + ": " + actual + " !== " + expected);
}

async function rejects(action, label) {
  try { await action(); } catch (error) { eq(error instanceof TypeError, true, label + " TypeError"); return; }
  throw new Error(label + " did not reject");
}

export async function run() {
  const text = await fetch("data:,ok");
  eq(text.status, 200, "data status");
  eq(text.type, "basic", "data type");
  eq(text.headers.get("content-type"), "text/plain;charset=US-ASCII", "data default type");
  eq(await text.text(), "ok", "data body");

  const base64 = await fetch("data:text/plain;base64,b2s=");
  eq(await base64.text(), "ok", "base64 body");
  eq(base64.headers.get("content-type"), "text/plain", "base64 type");

  const bytes = await fetch("data:;base64,AAEC");
  eq(new Uint8Array(await bytes.arrayBuffer()).join(","), "0,1,2", "binary data body");

  const blank = await fetch("about:blank");
  eq(blank.status, 200, "about status");
  eq(blank.headers.get("content-type"), "text/html;charset=UTF-8", "about type");
  eq(await blank.text(), "", "about body");

  const blob = new Blob(["hello"], { type: "text/x-test" });
  const url = URL.createObjectURL(blob);
  const fromBlob = await fetch(url);
  eq(fromBlob.status, 200, "blob status");
  eq(fromBlob.headers.get("content-type"), "text/x-test", "blob type");
  eq(await fromBlob.text(), "hello", "blob body");
  URL.revokeObjectURL(url);
  await rejects(() => fetch(url), "revoked blob");

  await rejects(() => fetch("file:///etc/passwd"), "file scheme");
  await rejects(() => fetch("ftp://example.test/x"), "ftp scheme");
  await rejects(() => fetch("http://example.test/"), "http without service");

  const controller = new AbortController();
  const marker = { reason: true };
  controller.abort(marker);
  let aborted = null;
  try { await fetch("data:,x", { signal: controller.signal }); } catch (error) { aborted = error; }
  eq(aborted, marker, "pre-aborted signal reason");
  return "ok";
}
