// @rejected "AggregateError: All promises were rejected"

async function run() {
  return await Promise.any([Promise.reject(1), Promise.reject(2)]);
}
