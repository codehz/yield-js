// @pending ["wait:key coercion"]
// @restore
// @fulfill ["wait:key coercion", 5]
// @fulfilled true

import { wait } from 'test';

export async function run() {
  const symbol = Symbol("saved key");
  const failure = {};
  let conversions = 0;
  const key = {
    toString() {
      conversions++;
      if (conversions > 1) throw failure;
      return symbol;
    }
  };
  const target = { [symbol]: 2 };
  let sum;
  try {
    sum = (target[key] += await wait(conversions === 1 ? "key coercion" : "unconverted key"));
    if (sum !== 7 || target[symbol] !== 7 || conversions !== 1) return false;
    target[key];
    return false;
  } catch (error) {
    return error === failure && conversions === 2 && sum === 7 && target[symbol] === 7;
  }
}
