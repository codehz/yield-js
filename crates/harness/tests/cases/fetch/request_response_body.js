// @fulfilled true
// Fetch API Body mixin; see WPT fetch/api/response/response-consume.html and response-clone.html.

export async function run() {
  const input = new Uint8Array([1, 65, 66, 2]);
  const response = new Response(input.subarray(1, 3), { status: 201, statusText: 'Created', headers: { 'X-Test': 'first' } });
  input[1] = 0;
  if (!response.ok || response.status !== 201 || response.statusText !== 'Created' || response.type !== 'default') return false;
  if (response.headers !== response.headers || response.url !== '' || response.redirected || response.bodyUsed) return false;
  const originalStream = response.body;
  const copy = response.clone();
  if (response.body === originalStream || copy.body === response.body) return false;
  copy.headers.set('x-test', 'second');
  if (response.headers.get('x-test') !== 'first') return false;
  const reader = response.body.getReader({ mode: 'byob' });
  if (response.bodyUsed) return false;
  try { await response.text(); return false; } catch (e) { if (!(e instanceof TypeError)) return false; }
  const result = await reader.read(new Uint8Array(2));
  if (result.done || result.value[0] !== 65 || result.value[1] !== 66 || !response.bodyUsed) return false;
  reader.releaseLock();
  if (await copy.text() !== 'AB' || !copy.bodyUsed) return false;
  try { await copy.bytes(); return false; } catch (e) { if (!(e instanceof TypeError)) return false; }

  const empty = new Response(null);
  if (empty.body !== null || await empty.text() !== '' || empty.bodyUsed || (await empty.bytes()).length !== 0) return false;
  if (new Response('').body === null) return false;
  const json = Response.json({ test: true }, { status: 202 });
  if (json.status !== 202 || json.headers.get('content-type') !== 'application/json' || !(await json.json()).test) return false;
  const redirect = Response.redirect('https://example.com/path', 307);
  if (redirect.status !== 307 || redirect.headers.get('location') !== 'https://example.com/path' || redirect.body !== null) return false;
  const error = Response.error();
  if (error.type !== 'error' || error.status !== 0 || error.ok || error.body !== null) return false;
  try { error.headers.set('x', '1'); return false; } catch (e) { if (!(e instanceof TypeError)) return false; }
  try { new Response('', { status: 204 }); return false; } catch (e) { if (!(e instanceof TypeError)) return false; }
  try { new Response(null, { status: 600 }); return false; } catch (e) { if (!(e instanceof RangeError)) return false; }
  try { await Response.prototype.text.call({}); return false; } catch (e) { if (!(e instanceof TypeError)) return false; }

  const form = new FormData();
  form.append('same', 'one');
  form.append('same', new File(['payload'], 'file.txt', { type: 'text/plain' }));
  form.append('same', 'two');
  const multipart = new Response(form);
  if (!multipart.headers.get('content-type').startsWith('multipart/form-data; boundary=')) return false;
  const all = (await multipart.formData()).getAll('same');
  if (all.length !== 3 || all[0] !== 'one' || all[2] !== 'two' || !(all[1] instanceof File)) return false;
  if (all[1].name !== 'file.txt' || all[1].type !== 'text/plain' || await all[1].text() !== 'payload') return false;
  const params = new URLSearchParams('x=1&x=2&space=a+b');
  const decoded = await new Response(params).formData();
  if (decoded.getAll('x').join(',') !== '1,2' || decoded.get('space') !== 'a b') return false;
  try { await new Response('not form data').formData(); return false; } catch (e) { return e instanceof TypeError; }
}
