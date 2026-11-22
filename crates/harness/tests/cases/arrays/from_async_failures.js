// @fulfilled true

export async function run() {
  const reason = {};
  let closed = 0;
  let doneValueRead = 0;
  const sync = {
    [Symbol.iterator]() {
      return {
        next() { return { done: false, value: Promise.reject(reason) }; },
        return() { closed++; throw "ignored cleanup failure"; }
      };
    }
  };
  try { await Array.fromAsync(sync); return false; }
  catch (error) { if (error !== reason || closed !== 1) return false; }

  const done = {
    [Symbol.iterator]() {
      return {
        next() {
          return { done: true, get value() { doneValueRead++; return Promise.reject(reason); } };
        },
        return() { closed++; return {}; }
      };
    }
  };
  try { await Array.fromAsync(done); return false; }
  catch (error) { if (error !== reason || closed !== 1 || doneValueRead !== 1) return false; }

  const invalid = {
    [Symbol.asyncIterator]() {
      return {
        next() { return Promise.reject(reason); },
        return() { closed++; return {}; }
      };
    }
  };
  try { await Array.fromAsync(invalid); return false; }
  catch (error) { if (error !== reason || closed !== 1) return false; }

  function Frozen() { return Object.freeze({}); }
  const nonWritable = {
    [Symbol.iterator]() {
      return {
        next() { return { done: false, value: 1 }; },
        return() { closed++; return { done: true }; }
      };
    }
  };
  try { await Array.fromAsync.call(Frozen, nonWritable); return false; }
  catch (error) { return error instanceof TypeError && closed === 2; }
}
