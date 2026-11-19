// @fulfilled true

export async function run() {
  let hints = "";
  const arg = {
    [Symbol.toPrimitive](hint) {
      if (this !== arg) throw "wrong receiver";
      hints += hint + ";";
      return "converted";
    }
  };
  if (Error(arg).message !== "converted" || new TypeError(arg).message !== "converted") return false;
  const fresh = Symbol(arg);
  if (Symbol.keyFor(fresh) !== undefined || Symbol.for(arg) !== Symbol.for("converted")) return false;
  if (hints !== "string;string;string;string;") return false;
  if (Object.getOwnPropertyDescriptor(Error(undefined), "message") !== undefined) return false;
  if (Symbol.keyFor(Symbol.for()) !== "undefined") return false;

  const marker = {};
  const throwing = { get toString() { throw marker; } };
  try { new Error(throwing); return false; } catch (e) { if (e !== marker) return false; }
  try { Symbol(throwing); return false; } catch (e) { if (e !== marker) return false; }
  try { Symbol.for(throwing); return false; } catch (e) { if (e !== marker) return false; }
  const symbol = { toString() { return fresh; } };
  try { TypeError(symbol); return false; } catch (e) { if (e.name !== "TypeError") return false; }
  try { Symbol.for(symbol); return false; } catch (e) { if (e.name !== "TypeError") return false; }
  return true;
}
