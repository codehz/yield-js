// @returns true

export function run() {
  function receiver() { return this; }
  let o = {};
  o.f = receiver;
  let detached = o.f;
  return ((o.f))() === o && (o["f"])() === o &&
    (detached)() === undefined;
}
