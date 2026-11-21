// @uncaught []

export function run() {
  queueMicrotask(() => 0);
  queueMicrotask(() => {
    try {
      throw "caught";
    } catch (error) {
      return error;
    }
  });
  return 0;
}
