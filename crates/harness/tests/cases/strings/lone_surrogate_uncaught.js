// @uncaught [{"utf16":[55296,65533]}, undefined]

export function run() {
  queueMicrotask(() => { throw "\uD800\uFFFD"; });
  queueMicrotask(() => { throw undefined; });
  return true;
}
