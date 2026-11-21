// @uncaught ["first", 2]

export function run() {
  Promise.reject('separate rejection notification');
  queueMicrotask(() => { throw "first"; });
  queueMicrotask(() => 0);
  queueMicrotask(() => { throw 2; });
  return 0;
}
