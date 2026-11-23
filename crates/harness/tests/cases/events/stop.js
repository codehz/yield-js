// @returns true

export function run() {
  const target = new EventTarget();
  const trace = [];
  function stop(event) { trace.push("stop"); event.stopPropagation(); }
  target.addEventListener("x", stop, true);
  target.addEventListener("x", () => trace.push("capture"), true);
  target.addEventListener("x", () => trace.push("bubble"));
  const event = new Event("x");
  target.dispatchEvent(event);
  target.removeEventListener("x", stop, true);
  target.dispatchEvent(event);
  target.addEventListener("x", event => event.stopImmediatePropagation(), true);
  target.addEventListener("x", () => trace.push("never"), true);
  target.dispatchEvent(event);
  return trace.join(",") === "stop,capture,capture,bubble,capture" && !event.cancelBubble;
}
