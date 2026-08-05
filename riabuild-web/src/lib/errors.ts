/**
 * Convex frames a thrown error with its own prefix before it reaches the client.
 * The first line after that prefix is the sentence a developer wrote for a human
 * to read; everything after it is a stack trace nobody wants in a panel.
 */
export function readError(
  cause: unknown,
  fallback = "Something went wrong.",
): string {
  if (!(cause instanceof Error)) return fallback;
  const message = cause.message
    .replace(/^.*Uncaught Error:\s*/, "")
    .split("\n")[0];
  return message.trim() === "" ? fallback : message;
}
