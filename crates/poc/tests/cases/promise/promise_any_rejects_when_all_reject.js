// @rejected "AggregateError: All promises were rejected"

export async function run() {
  return await Promise.any([Promise.reject(1), Promise.reject(2)]);
}
