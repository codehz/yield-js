exports.n = 1;
const b = require('./b.cjs');
exports.seen = b.seen;
exports.same = b === require('./b.cjs');
module.exports.n = 2;
