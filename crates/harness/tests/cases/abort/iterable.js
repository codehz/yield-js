// @returns true

// Iterator conversion must finish even when the first item is aborted.
export function run() {
  const reason = {};
  const source = AbortSignal.abort(reason);
  let closed = false;
  function* invalid() {
    try {
      yield source;
      yield {};
    } finally {
      closed = true;
    }
  }
  try { AbortSignal.any(invalid()); }
  catch (error) {
    return error instanceof TypeError && closed && AbortSignal.any(new Set([source])).reason === reason;
  }
  return false;
}
