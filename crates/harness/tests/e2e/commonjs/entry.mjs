import a, { n, seen, same } from './shim.cjs';
import b from './b.cjs';

export function run() {
  if (b.a !== a || !same || a.n !== n || seen !== b.seen) return -1;
  return seen * 10 + n;
}
