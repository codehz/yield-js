// @pending ["wait:reflect"]
// @restore
// @fulfill ["wait:reflect", 0]
// @fulfilled true

import { wait } from 'test';

function Base() {}
Base.prototype = { inherited: 1, get seen() { return this.receiver; } };
Object.defineProperty(Base.prototype, "locked", { value: 1 });

export async function run() {
  const base = new Base();
  const receiver = { receiver: "child" };
  let trace = "";
  const target = Object.create(base, {
    value: { value: 2, writable: true, enumerable: true, configurable: true },
    get: { get() { return this.receiver; }, configurable: true },
    set: { set(value) { this.written = value; }, configurable: true },
    key: { get() { trace += "get;"; throw "getter"; }, configurable: true },
    primitive: { set(value) { if (this === undefined) trace += value; } },
    failure: { set(value) { throw value; } },
  });
  const array = [1];
  Object.defineProperty(array, "1", { value: 2, writable: false, configurable: false });
  const get = Reflect.get;
  const has = Reflect.has;
  const set = Reflect.set;
  const descriptor = Reflect.getOwnPropertyDescriptor;
  await wait("reflect");

  if (get(target, "inherited") !== 1 || get(target, "get") !== undefined ||
      get(target, "seen", receiver) !== "child" || get(target, "value", undefined) !== 2) return false;
  if (has(target, "inherited") !== true || has(target, "missing") !== false ||
      has(target, "key") !== true || typeof descriptor(target, "key").get !== "function" || trace !== "") return false;
  if (set(target, "value", 3, undefined) !== false || target.value !== 2 ||
      set(target, "value", 3, receiver) !== true || receiver.value !== 3 || target.value !== 2) return false;
  if (set(target, "set", 4, receiver) !== true || receiver.written !== 4 || "written" in target) return false;
  if (set(target, "locked", 5, receiver) !== false || receiver.locked !== undefined ||
      set(target, "value", 0, Object.preventExtensions({})) !== false ||
      set(target, "value", 0, { set value(v) { throw "wrong setter"; } }) !== false) return false;
  if (set(target, "primitive", "primitive;", undefined) !== true || trace !== "primitive;") return false;

  const own = descriptor(target, "value");
  if (own.value !== 2 || !own.writable || !own.enumerable || !own.configurable ||
      descriptor(target, "inherited") !== undefined) return false;
  if (descriptor(array, "length").value !== 2 || set(array, "length", 1) !== false ||
      array.length !== 2 || array[0] !== 1) return false;
  if (set(array, "1", 9) !== false || array[1] !== 2 || set(array, "2", 9) !== true || array[2] !== 9) return false;

  const symbol = Symbol("reflect");
  const probe = { [Symbol.toPrimitive]() { trace += "key;"; return symbol; } };
  if (!set(target, probe, 9) || !has(target, symbol) || get(target, symbol) !== 9 ||
      descriptor(target, symbol).value !== 9) return false;
  try { get(null, probe); return false; } catch (error) { trace += error.name + ";"; }
  try { get(target, "key"); return false; } catch (error) { trace += error + ";"; }
  try { set(target, "failure", "setter"); return false; } catch (error) { trace += error + ";"; }
  try { set(array, "length", -1); return false; } catch (error) { trace += error.name; }
  return trace === "primitive;key;TypeError;get;getter;setter;RangeError" && array.length === 3;
}
