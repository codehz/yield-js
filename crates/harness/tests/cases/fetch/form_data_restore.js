// @pending ["wait:form-data"]
// @restore
// @fulfill ["wait:form-data", null]
// @fulfilled true

import { gc, wait } from 'test';

function savedEntries() {
  const data = new FormData();
  data.append('first', 'consumed');
  data.append('next', new File(['saved'], 'saved.txt', { lastModified: 41 }));
  const entries = data.entries();
  entries.next();
  return entries;
}

export async function run() {
  const entries = savedEntries();
  await wait('form-data');
  gc();
  const saved = entries.next();
  if (saved.done || saved.value[0] !== 'next') return false;
  const file = saved.value[1];
  if (!(file instanceof File) || file.name !== 'saved.txt' || file.lastModified !== 41) return false;
  if (await file.text() !== 'saved' || !entries.next().done) return false;

  const data = new FormData();
  data.append('a', 1);
  data.append('b', 2);
  data.append('c', 3);
  const live = data[Symbol.iterator]();
  if (data[Symbol.iterator] !== data.entries || live[Symbol.iterator]() !== live) return false;
  if (Object.prototype.toString.call(live) !== '[object FormData Iterator]') return false;
  if (live.next().value.join(':') !== 'a:1') return false;
  data.delete('a');
  data.append('d', 4);
  if (live.next().value.join(':') !== 'c:3' || live.next().value.join(':') !== 'd:4') return false;
  if (!live.next().done) return false;
  data.append('e', 5);
  if (live.next().value.join(':') !== 'e:5') return false;

  const receiver = {};
  let seen = '';
  data.forEach(function (value, name, self) {
    if (this !== receiver || self !== data) throw new Error('incorrect forEach arguments');
    seen += name + value;
    if (name === 'b') {
      data.delete('b');
      data.append('f', 6);
    }
  }, receiver);
  if (seen !== 'b2d4e5f6') return false;
  let badBrand = false;
  try { live.next.call(data); } catch (e) { badBrand = e instanceof TypeError; }
  const marker = {};
  let abrupt = false;
  try { data.forEach(() => { throw marker; }); } catch (e) { abrupt = e === marker; }
  return badBrand && abrupt;
}
