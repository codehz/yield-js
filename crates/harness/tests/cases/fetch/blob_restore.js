// @pending ["wait:blob"]
// @restore
// @fulfill ["wait:blob", true]
// @fulfilled "ABCD|BC|BC|a/b|7|4"

import { wait } from 'test';

export async function run() {
  const backing = new Uint8Array([65, 66, 67]);
  const blob = new Blob([backing, "D"], { type: "X/Y" });
  const slice = blob.slice(1, 3);
  const file = new File([slice], "name", { type: "A/B", lastModified: 7 });
  backing.fill(0);
  await wait("blob");
  if (!(file instanceof Blob) || slice instanceof File) throw "brands changed";
  return [
    await blob.text(),
    await slice.text(),
    await file.text(),
    file.type,
    String(file.lastModified),
    String(blob.size),
  ].join("|");
}
