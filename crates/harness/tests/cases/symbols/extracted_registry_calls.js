// @fulfilled "ok"

export async function run() {
  const registryFor = Symbol.for;
  const registryKeyFor = Symbol.keyFor;
  const shared = registryFor("probe");
  if (shared !== Symbol.for("probe")) return "for identity";
  if (registryKeyFor(shared) !== "probe") return "keyFor";
  if (Symbol.for("probe") !== registryFor("probe")) return "method identity";
  return "ok";
}
