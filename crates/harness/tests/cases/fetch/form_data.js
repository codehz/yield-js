// @fulfilled true
// Adapted from WPT xhr/formdata/append.any.js and xhr/formdata/set.any.js.

export async function run() {
  const data = new FormData(undefined, null);
  if (globalThis.FormData !== FormData || FormData.length !== 0) return false;
  if (Object.prototype.toString.call(data) !== '[object FormData]') return false;
  data.append('x', 1);
  data.append('y', undefined);
  data.append('x', null);
  data.append('z', true);
  if (data.get('x') !== '1' || data.getAll('x').join(',') !== '1,null') return false;
  if (data.get('missing') !== null || data.getAll('missing').length !== 0) return false;
  if (data.get('y') !== 'undefined' || !data.has('y')) return false;
  data.set('x', 'replacement');
  data.set('new', 5);
  if ([...data.keys()].join(',') !== 'x,y,z,new') return false;
  if ([...data.values()].join(',') !== 'replacement,undefined,true,5') return false;
  data.append('x', 'another');
  data.delete('x');
  if (data.has('x') || data.getAll('x').length !== 0) return false;
  let conversionOrder = '';
  data.append({ toString() { conversionOrder += 'name'; return '\ud800'; } },
              { toString() { conversionOrder += ',value'; return '\udc00'; } });
  if (conversionOrder !== 'name,value' || data.get('\ufffd') !== '\ufffd') return false;

  const blob = new Blob(['pre', 'text', 'post'], { type: 'text/plain' });
  data.append('file', blob.slice(3, 7, blob.type));
  const converted = data.get('file');
  if (!(converted instanceof File) || converted.name !== 'blob' || converted.type !== 'text/plain') return false;
  if (converted.size !== 4 || await converted.text() !== 'text') return false;
  const original = new File(['original'], 'original.txt', { type: 'text/plain', lastModified: 17 });
  data.append('file', original, undefined);
  if (data.getAll('file')[1] !== original) return false;
  data.set('file', original, { toString() { return 'new\ud800.txt'; } });
  const renamed = data.get('file');
  if (!(renamed instanceof File) || renamed === original || renamed.name !== 'new\ufffd.txt') return false;
  if (original.name !== 'original.txt' || renamed.lastModified !== 17 || renamed.type !== original.type) return false;
  if (data.getAll('file').length !== 1 || await renamed.text() !== 'original') return false;

  let errors = 0;
  try { FormData(); } catch (e) { if (e instanceof TypeError) errors++; }
  try { new FormData({}); } catch (e) { if (e instanceof TypeError) errors++; }
  try { new FormData(null); } catch (e) { if (e instanceof TypeError) errors++; }
  try { data.set('file'); } catch (e) { if (e instanceof TypeError) errors++; }
  try { data.append('file', 'text', 'filename'); } catch (e) { if (e instanceof TypeError) errors++; }
  try { data.set('file', Symbol()); } catch (e) { if (e instanceof TypeError) errors++; }
  try { data.set('file', blob, Symbol()); } catch (e) { if (e instanceof TypeError) errors++; }
  try { FormData.prototype.get.call(Object.create(FormData.prototype), 'file'); } catch (e) { if (e instanceof TypeError) errors++; }
  try { data.forEach({}); } catch (e) { if (e instanceof TypeError) errors++; }
  return errors === 9 && data.get('file') === renamed;
}
