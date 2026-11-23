// @restore
// @advance 4
// @fulfilled true

import { sleep } from "test";

// Constructor and dictionary-order assertions adapted from WPT
// dom/events/Event-constructors.any.js.
const fields = [];
const detail = {};
const event = new CustomEvent("example", {
  get detail() { fields.push("detail"); return detail; },
  get composed() { fields.push("composed"); return true; },
  get cancelable() { fields.push("cancelable"); return true; },
  get bubbles() { fields.push("bubbles"); return false; },
});
export async function run() {
  await sleep(4);
  const later = new Event("later");
  return fields.join(",") === "bubbles,cancelable,composed,detail" &&
    event instanceof CustomEvent && event instanceof Event && event.detail === detail &&
    Object.getPrototypeOf(CustomEvent) === Event && globalThis instanceof EventTarget &&
    event.target === null && event.srcElement === null && event.currentTarget === null &&
    event.eventPhase === Event.NONE && !event.bubbles && event.composed && event.cancelable &&
    event.returnValue && !event.defaultPrevented && !event.isTrusted &&
    Object.getOwnPropertyDescriptor(event, "isTrusted").configurable === false &&
    later.timeStamp - event.timeStamp === 4;
}
