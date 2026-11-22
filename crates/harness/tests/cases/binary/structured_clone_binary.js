// @returns true
import { expect, gc } from 'test';

function thrown(callback) {
  try {
    callback();
  } catch (error) {
    return error;
  }
  throw 'expected an exception';
}

export function run() {
  const buffer = new ArrayBuffer(8, { maxByteLength: 16 });
  const tracking = new Uint8Array(buffer, 2);
  const fixed = new DataView(buffer, 2, 4);
  fixed.setUint8(0, 17);
  // Visit a view before the buffer and check both references use the same clone.
  const copy = structuredClone({ tracking, fixed, buffer });
  expect(copy.buffer === buffer).toBe(false);
  expect(copy.tracking.buffer).toBe(copy.buffer);
  expect(copy.fixed.buffer).toBe(copy.buffer);
  expect(copy.tracking.byteOffset).toBe(2);
  expect(copy.fixed.byteOffset).toBe(2);
  expect(copy.tracking[0]).toBe(17);
  expect(copy.fixed.getUint8(0)).toBe(17);
  expect(copy.buffer.resizable).toBe(true);
  expect(copy.buffer.maxByteLength).toBe(16);
  copy.buffer.resize(12);
  expect(copy.tracking.length).toBe(10);
  expect(copy.fixed.byteLength).toBe(4);
  copy.fixed.setUint8(0, 19);
  expect(copy.tracking[0]).toBe(19);
  expect(tracking[0]).toBe(17);
  expect(tracking.length).toBe(6);
  expect(buffer.byteLength).toBe(8);

  const unused = new ArrayBuffer(1);
  const moved = structuredClone({
    tracking,
    buffer,
    get last() {
      expect(buffer.detached).toBe(false);
      tracking[0] = 23;
      gc();
      return true;
    },
  }, { transfer: [buffer, unused] });
  expect(buffer.detached).toBe(true);
  expect(unused.detached).toBe(true);
  expect(moved.buffer.detached).toBe(false);
  expect(moved.buffer.maxByteLength).toBe(16);
  expect(moved.tracking.buffer).toBe(moved.buffer);
  expect(moved.tracking[0]).toBe(23);
  expect(moved.last).toBe(true);

  const kept = new ArrayBuffer(1);
  let reads = 0;
  const sentinel = {};
  const input = { get value() { reads++; throw sentinel; } };
  expect(thrown(() => structuredClone(input, { transfer: [kept, kept] })).name).toBe('DataCloneError');
  expect(reads).toBe(0);
  expect(kept.detached).toBe(false);
  expect(thrown(() => structuredClone(input, { transfer: [kept] }))).toBe(sentinel);
  expect(reads).toBe(1);
  expect(kept.detached).toBe(false);

  const first = new ArrayBuffer(1);
  const late = new ArrayBuffer(1);
  expect(thrown(() => structuredClone({
    get value() {
      expect(first.detached).toBe(false);
      structuredClone(null, { transfer: [late] });
      return true;
    },
  }, { transfer: [first, late, kept] })).name).toBe('DataCloneError');
  expect(first.detached).toBe(true);
  expect(late.detached).toBe(true);
  expect(kept.detached).toBe(false);

  const order = [];
  const options = {
    get transfer() {
      order.push('transfer');
      return {
        [Symbol.iterator]() {
          order.push('iterator');
          let yielded = false;
          return {
            next() {
              order.push('next');
              if (yielded) return { done: true };
              yielded = true;
              return { value: 1, done: false };
            },
            return() { order.push('return'); return {}; },
          };
        },
      };
    },
  };
  expect(thrown(() => structuredClone(input, options)) instanceof TypeError).toBe(true);
  expect(order.join(',')).toBe('transfer,iterator,next,return');
  expect(reads).toBe(1);
  return true;
}
