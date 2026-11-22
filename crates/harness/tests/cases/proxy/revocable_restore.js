// @pending ["wait:live"]
// @restore
// @fulfill ["wait:live", 0]
// @pending ["wait:revoked"]
// @restore
// @fulfill ["wait:revoked", 0]
// @fulfilled "ok"

import { gc, wait } from "test";

function assertRevoked(proxy) {
  try {
    Array.isArray(proxy);
  } catch (error) {
    if (error instanceof TypeError) return;
    throw error;
  }
  throw "revoked proxy was still usable";
}

function orphanRevoker() {
  return Proxy.revocable({}, {}).revoke;
}

export async function run() {
  // This revoker alone owns the reference to its proxy, target, and handler.
  const orphan = orphanRevoker();
  const { proxy, revoke } = Proxy.revocable([42], {});
  const unaffected = Proxy.revocable({ x: 7 }, {}).proxy;
  gc();
  await wait("live");
  gc();
  if (!Array.isArray(proxy) || proxy[0] !== 42) throw "lost live proxy slots";
  if (orphan() !== undefined) throw "lost orphan revoker authority";
  if (revoke.call(unaffected) !== undefined) throw "wrong revoker result";
  assertRevoked(proxy);
  gc();
  await wait("revoked");
  gc();
  if (orphan() !== undefined || revoke() !== undefined) throw "revocation was not idempotent";
  assertRevoked(proxy);
  if (unaffected.x !== 7) throw "revoker affected an unrelated proxy";
  return "ok";
}
