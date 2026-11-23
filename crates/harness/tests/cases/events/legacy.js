// @returns true

export function run() {
  class Derived extends CustomEvent {}
  const value = {};
  const event = new Derived("old", { detail: value, composed: true, cancelable: true });
  event.returnValue = false;
  if (!event.defaultPrevented || event.returnValue) return false;
  event.returnValue = true;
  if (!event.defaultPrevented) return false;
  event.cancelBubble = true;
  event.cancelBubble = false;
  if (!event.cancelBubble) return false;
  event.initCustomEvent("new", true, true, value);
  if (event.cancelBubble || event.defaultPrevented || event.type !== "new" ||
      !event.bubbles || !event.composed || event.detail !== value || !(event instanceof Derived)) return false;
  try { Object.getOwnPropertyDescriptor(Event.prototype, "type").get.call(Object.create(Event.prototype)); }
  catch (error) {
    if (!(error instanceof TypeError)) return false;
    try { structuredClone(event); }
    catch (cloneError) { return cloneError.name === "DataCloneError"; }
  }
  return false;
}
