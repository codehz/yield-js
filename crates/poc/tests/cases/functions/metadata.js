// @returns true

function declared(first, {value}, third = 0, after, ...rest) {}

export function run() {
  const expression = function explicit(a, ...rest) {};
  const anonymous = function(a) {};
  const arrow = (a, b = 0, ...rest) => 0;
  const restOnly = function(...args) {};
  const asyncFunction = async function namedAsync(a, b) {};
  const methods = {method(a, ...rest) {}, ['computed'](a, b) {}};
  return declared.name === 'declared' && declared.length === 2 &&
    expression.name === 'explicit' && expression.length === 1 &&
    anonymous.name === 'function' && anonymous.length === 1 &&
    arrow.name === 'arrow' && arrow.length === 1 && restOnly.length === 0 &&
    asyncFunction.name === 'namedAsync' && asyncFunction.length === 2 &&
    methods.method.name === 'method' && methods.method.length === 1 &&
    methods.computed.name === 'computed' && methods.computed.length === 2;
}
