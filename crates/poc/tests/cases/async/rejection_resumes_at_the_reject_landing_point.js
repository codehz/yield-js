// @args ["input"]
// @restore
// @reject ["wait:a","boom"]
// @pending []
// @rejected "boom"

import { inspect, seed, wait } from 'test';

export async function run(input) {
  inspect(input);
  const base = seed();
  const x = await wait("a");
  const sum = base + x;
  const y = await wait("b");
  return sum + y;
}
