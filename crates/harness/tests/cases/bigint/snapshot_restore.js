// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 7]
// @fulfilled "1606938044258990275541962092341162602522202993782792835301376:1606938044258990275541962092341162602522202993782792835301383:bigint"

import { expect, wait } from 'test';

export async function run() {
  const big = 2n ** 200n;
  const holder = { big, boxed: Object(7n), map: new Map([[9n, 11n]]) };
  const matcher = expect(big);
  await wait('a');
  expect(matcher.value).toBe(big);
  expect(holder.big).toBe(2n ** 200n);
  expect(holder.boxed.valueOf()).toBe(7n);
  expect(holder.boxed instanceof BigInt).toBe(true);
  expect(holder.map.get(9n)).toBe(11n);
  return [
    holder.big.toString(),
    (holder.big + 7n).toString(),
    typeof holder.big,
  ].join(':');
}
