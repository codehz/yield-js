// @returns "number,number,false,true,true"

export function run() {
  const types = typeof NaN === "number" && typeof Infinity === "number";
  const parts = [
    `${typeof NaN}`,
    `${typeof Infinity}`,
    `${NaN === NaN}`,
    `${Infinity > 1e308}`,
    `${-Infinity < -1e308}`,
  ];
  if (!types || 1 / Infinity !== 0) {
    return "failed";
  }
  return parts.join(",");
}
