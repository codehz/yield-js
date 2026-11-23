// @returns true

// Dictionary member order and null-callback conversion adapted from WPT
// dom/events/AddEventListenerOptions-passive.any.js.
export function run() {
  const target = new EventTarget();
  const trace = [];
  const options = {
    get passive() { trace.push("passive"); return true; },
    get once() { trace.push("once"); return true; },
    get capture() { trace.push("capture"); return true; },
    get unrelated() { throw "unexpected getter"; },
  };
  target.addEventListener("x", null, options);
  target.removeEventListener("x", null, options);
  return trace.join(",") === "capture,once,passive,capture";
}
