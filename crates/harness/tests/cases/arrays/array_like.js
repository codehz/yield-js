// @returns undefined

import { expect } from 'test';

export function run() {
  const a = Array.prototype;
  expect(a.at.call({length:4294967297, 4294967296:'x'}, -1)).toBe('x');
  let boxed;
  expect(a.map.call('ab', (v, i, source) => {
    if (i === 0) boxed = source;
    expect(source).toBe(boxed);
    expect(typeof source).toBe('object');
    return v;
  }).join('')).toBe('ab');
  function fromArgs() { return a.slice.call(arguments); }
  expect(fromArgs(1,2).join()).toBe('1,2');
  expect(a.map.call({}, () => { throw 'unexpected callback'; }).length).toBe(0);
  let error;
  try { a.map.call(null, () => 0); } catch (e) { error = e.name; }
  expect(error).toBe('TypeError');

  const log = [];
  const proto = {};
  const target = [1,2,,4];
  Object.setPrototypeOf(target, proto);
  const proxy = new Proxy(target, {
    has(t,k) { log.push('h'+k); return Reflect.has(t,k); },
    get(t,k,r) { log.push('g'+k); return Reflect.get(t,k,r); }
  });
  const mapped = a.map.call(proxy, (v,i) => {
    if (i === 0) { delete target[1]; proto[1] = 8; }
    return v * 2;
  });
  expect(mapped.join()).toBe('2,16,,8');
  expect(log.join()).toBe('glength,gconstructor,h0,g0,h1,g1,h2,h3,g3');
  log.length = 0;
  expect(a.findLast.call(proxy, v => v === undefined)).toBe(undefined);
  expect(log.join()).toBe('glength,g3,g2');
  expect(a.findLastIndex.call(proxy, v => v === 8)).toBe(1);
  expect([1,,2,3].reduceRight((s,v) => s*10+v)).toBe(321);
  expect([,1].reduceRight((s,v) => String(s)+v,undefined)).toBe('undefined1');
  try { Array(2).reduceRight(() => 0); } catch (e) { error = e.name; }
  expect(error).toBe('TypeError');

  expect([1,2,3,4].copyWithin(1,0,3).join()).toBe('1,1,2,3');
  const sparse = {0:1,2:3,3:4,length:4};
  const trapped = new Proxy(sparse, {
    set(t,k,v) { log.push('s'+k); t[k] = v; return true; },
    deleteProperty(t,k) { log.push('d'+k); return false; }
  });
  log.length = 0; error = undefined;
  try { a.copyWithin.call(trapped, 2, 0, 2); } catch (e) { error = e.name; }
  expect(log.join()).toBe('s2,d3');
  expect(error).toBe('TypeError');
  expect(sparse[2]).toBe(1);
  log.length = 0; error = undefined;
  try { a.reverse.call(trapped); } catch (e) { error = e.name; }
  expect(log.join()).toBe('s0,s3,s1,d2');
  expect(error).toBe('TypeError');
  expect(sparse[0]).toBe(4);
  expect(sparse[1]).toBe(1);
  const lowerOnly = new Proxy({0:7,length:2}, {
    deleteProperty(t,k) { log.push('d'+k); return Reflect.deleteProperty(t,k); },
    set(t,k,v) { log.push('s'+k); t[k] = v; return true; }
  });
  log.length = 0;
  expect(a.reverse.call(lowerOnly)).toBe(lowerOnly);
  expect(log.join()).toBe('d0,s1');
}
