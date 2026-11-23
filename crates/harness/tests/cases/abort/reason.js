// @returns true

// WPT dom/abort/event.any.js: AbortSignal reason identity and trusted abort.
export function run() {
  const controller = new AbortController();
  const signal = controller.signal;
  const trace = [];
  let trusted = false;
  function handler(event) { trace.push("handler"); trusted = event.isTrusted; }
  signal.addEventListener("abort", () => trace.push("first"));
  signal.onabort = () => trace.push("replaced");
  signal.addEventListener("abort", handler);
  signal.onabort = handler;
  signal.addEventListener("abort", () => trace.push("last"));
  signal.removeEventListener("abort", handler);
  if (signal.onabort !== handler || signal.aborted || signal.reason !== undefined) return false;
  controller.abort();
  controller.abort(null);
  const reason = signal.reason;
  if (!(reason instanceof DOMException) || reason.name !== "AbortError" || reason.code !== 20 ||
      !signal.aborted || !trusted || trace.join(",") !== "first,handler,last" ||
      Object.getPrototypeOf(AbortSignal) !== EventTarget || !(signal instanceof EventTarget)) return false;
  const first = AbortSignal.abort(null), second = AbortSignal.abort(42);
  if (AbortSignal.any([first, second]).reason !== null || AbortSignal.any([second, first]).reason !== 42) return false;
  try { new AbortSignal(); }
  catch (error) { return error instanceof TypeError && signal.reason === reason; }
  return false;
}
