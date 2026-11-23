// @pending ["wait:request-body"]
// @restore
// @fulfill ["wait:request-body", null]
// @fulfilled true

import { wait, gc } from 'test';

export async function run() {
  const controller = new AbortController();
  const marker = {};
  const request = new Request('https://example.test/path#fragment', {
    method: 'post', body: 'initial', signal: controller.signal, credentials: 'include',
  });
  const copy = request.clone();
  if (request.method !== 'POST' || request.url !== 'https://example.test/path#fragment') return false;
  if (copy.credentials !== 'include' || copy.headers.get('content-type') !== 'text/plain;charset=UTF-8') return false;
  if (copy.signal === request.signal || request.signal === controller.signal) return false;
  controller.abort(marker);
  if (copy.signal.reason !== marker || request.signal.reason !== marker) return false;
  if (await request.text() !== 'initial' || !request.bodyUsed || await copy.text() !== 'initial') return false;
  try { request.clone(); return false; } catch (e) { if (!(e instanceof TypeError)) return false; }
  try { new Request('https://example.test/', { body: '' }); return false; } catch (e) { if (!(e instanceof TypeError)) return false; }
  try { new Request('https://user@example.test/'); return false; } catch (e) { if (!(e instanceof TypeError)) return false; }
  try { new Request('https://example.test/', { method: 'tRaCe' }); return false; } catch (e) { if (!(e instanceof TypeError)) return false; }
  try { new Request('https://example.test/', { cache: 'only-if-cached' }); return false; } catch (e) { if (!(e instanceof TypeError)) return false; }
  const previous = new Request('https://example.test/', { method: 'POST', body: 'transferred' });
  const transferred = new Request(previous);
  if (!previous.bodyUsed || await transferred.text() !== 'transferred') return false;

  let streamController;
  const stream = new ReadableStream({ start(value) { streamController = value; } });
  try { new Request('https://example.test/', { method: 'POST', body: stream }); return false; } catch (e) { if (!(e instanceof TypeError)) return false; }
  const streaming = new Request('https://example.test/', { method: 'POST', body: stream, duplex: 'half' });
  if (streaming.body !== stream || streaming.duplex !== 'half') return false;
  const text = streaming.text();
  if (!streaming.bodyUsed) return false;
  streamController.enqueue(new Uint8Array([65]));
  await wait('request-body');
  gc();
  streamController.enqueue(new Uint8Array([66]));
  streamController.close();
  return await text === 'AB' && !streaming.body.locked;
}
