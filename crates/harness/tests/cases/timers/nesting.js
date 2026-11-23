// @advance 0
// @advance 3
// @advance 1
// @fulfilled "1@0,2@0,3@0,4@0,5@0,6@0,M@0,check@3,7@4"

export function run() {
  const order = [];
  const stamp = label => order.push(label + '@' + Date.now());
  return new Promise(resolve => {
    function tick(n) {
      stamp(n);
      if (n === 7) {
        resolve(order.join(','));
      } else {
        setTimeout(tick, 0, n + 1);
        if (n === 6) queueMicrotask(() => setTimeout(() => stamp('M'), 0));
      }
    }
    setTimeout(tick, 0, 1);
    setTimeout(() => stamp('check'), 3);
  });
}
