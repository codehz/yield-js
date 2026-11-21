// @pending ["wait:value"]
// @restore
// @fulfill ["wait:value", 7]
// @fulfilled "7|true|0"
import { wait } from 'test';

export async function run() {
  let steps = 0;
  let closed = 0;
  const iterable = {
    [Symbol.iterator]() {
      return {
        next() {
          steps += 1;
          if (steps === 1) return { value: wait("value"), done: false };
          return { done: true };
        },
        return() { closed += 1; return {}; },
      };
    },
  };
  let text = "";
  for await (const value of iterable) text += value;
  return `${text}|${steps === 2}|${closed}`;
}
