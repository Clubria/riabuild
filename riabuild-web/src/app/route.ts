export type Route =
  | { kind: "dashboard" }
  | { kind: "authorize" }
  | { kind: "gallery" }
  | { kind: "notFound"; path: string };

/**
 * Still no router — the product has three destinations and a library would be
 * more moving parts than it has pages. But the list of valid paths is now
 * explicit, because a 404 is only possible once something knows what is valid.
 *
 * The gallery resolves to a 404 outside dev builds, so guessing the path in
 * production finds nothing.
 */
export function route(pathname: string): Route {
  const path = normalise(pathname);

  if (path === "/") return { kind: "dashboard" };
  // Short because a developer types it by hand, off a terminal that may be on
  // another machine entirely.
  if (path === "/cli") return { kind: "authorize" };
  if (path === "/__ui" && import.meta.env.DEV) return { kind: "gallery" };

  return { kind: "notFound", path };
}

function normalise(pathname: string): string {
  const trimmed = pathname.replace(/\/+$/, "");
  return trimmed === "" ? "/" : trimmed;
}
