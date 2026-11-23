// @uncaught ["getter", "after", "microtask"]

export function run() {
  const target = new EventTarget();
  target.addEventListener("x", {
    get handleEvent() { throw "getter"; },
  });
  target.addEventListener("x", () => {
    queueMicrotask(() => { throw "microtask"; });
  });
  target.addEventListener("x", () => { throw "after"; });
  target.dispatchEvent(new Event("x"));
}
