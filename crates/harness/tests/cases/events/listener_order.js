// @restore
// @returns true

import { gc } from "test";

// Target ordering and same-event reentrancy adapted from WPT
// dom/events/EventTarget-dispatchEvent.html.
const target = new EventTarget();
const log = [];
let valid = true;
function removed() { log.push("moved"); }
target.addEventListener("x", function(event) {
  valid = valid && this === target && event.target === target &&
    event.currentTarget === target && event.eventPhase === Event.AT_TARGET &&
    event.composedPath()[0] === target;
  try { target.dispatchEvent(event); valid = false; }
  catch (error) { valid = valid && error.name === "InvalidStateError"; }
  log.push("capture");
  gc();
}, true);
target.addEventListener("x", function(event) {
  log.push("once");
  target.removeEventListener("x", removed);
  target.addEventListener("x", removed);
  target.dispatchEvent(new Event("x"));
  event.preventDefault();
}, { once: true });
target.addEventListener("x", removed);
const listener = {
  get handleEvent() {
    return function(event) {
      valid = valid && this === listener;
      event.preventDefault();
      log.push("passive");
    };
  },
};
target.addEventListener("x", listener, { passive: true });
target.addEventListener("x", listener, { once: true });

export function run() {
  const event = new Event("x", { cancelable: true });
  const first = target.dispatchEvent(event);
  const second = target.dispatchEvent(new Event("x", { cancelable: true }));
  return valid && !first && second && event.defaultPrevented && event.target === target &&
    event.currentTarget === null && event.eventPhase === Event.NONE &&
    event.composedPath().length === 0 &&
    log.join(",") === "capture,once,capture,passive,moved,passive,capture,passive,moved";
}
