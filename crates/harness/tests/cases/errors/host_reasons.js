// @pending ["reject:r", "wait:a"]
// @restore
// @reject ["wait:a", "boom"]
// @fulfilled true

import { expect, wait, rejectAfterRestore } from 'test';

export async function run() {
  // Matcher failures are ordinary branded Errors carrying the original
  // message, not raw strings.
  let matcherFailure;
  try { expect(1).toBe(2); } catch (error) { matcherFailure = error; }
  expect(Error.isError(matcherFailure)).toBe(true);
  expect(matcherFailure.message).toBe('toBe: expected 2, actual 1');
  expect(matcherFailure instanceof Error).toBe(true);

  // An explicit throw keeps the thrown value's identity.
  const token = { thrown: true };
  let caught;
  try { throw token; } catch (error) { caught = error; }
  expect(caught === token).toBe(true);

  // SuppressedError without a message still carries the brand, keeps both
  // values, and defines no own message property.
  const inner = new Error('inner');
  const outer = new Error('outer');
  const suppressed = new SuppressedError(inner, outer);
  expect(Error.isError(suppressed)).toBe(true);
  expect(suppressed.error === inner).toBe(true);
  expect(suppressed.suppressed === outer).toBe(true);
  expect(Object.keys(suppressed).includes('message')).toBe(false);
  expect(suppressed.stack.startsWith('SuppressedError: ')).toBe(true);

  const restoreRejection = rejectAfterRestore('r');
  let waitReason;
  try { await wait('a'); } catch (reason) { waitReason = reason; }
  // Host-provided reject reasons stay the exact values the guest gave them.
  expect(waitReason === 'boom').toBe(true);

  // The restore strategy synthesizes its rejection reason as a branded
  // Error carrying the declared message.
  let restoreReason;
  try { await restoreRejection; } catch (reason) { restoreReason = reason; }
  expect(Error.isError(restoreReason)).toBe(true);
  expect(restoreReason.message).toBe('reject rejected during restore');
  expect(restoreReason instanceof Error).toBe(true);
  return true;
}
