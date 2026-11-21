// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 1]
// @fulfilled "1:false,3:false,9:true:1"

import { wait } from 'test';

export async function run() {
  let ranFinally = 0;
  async function* g() {
    try {
      yield await wait("a");
      yield 2;
    } finally {
      ranFinally = ranFinally + 1;
      yield 3;
    }
  }
  const it = g();
  // Queue all three before the awaited operand settles, so the snapshot cut
  // contains an in-progress head request and two waiting behind it.
  const first = it.next();
  const second = it.return(9);
  const third = it.next();
  const results = [await first, await second, await third];
  return (
    results.map((result) => `${result.value}:${result.done}`).join(",") +
    ":" +
    ranFinally
  );
}
