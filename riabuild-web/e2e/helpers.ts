import AxeBuilder from "@axe-core/playwright";
import { expect, Page, test as base, TestInfo } from "@playwright/test";

/**
 * A page that remembers what the console said.
 *
 * Console errors have to be collected from before the first navigation, which
 * means a fixture rather than a helper called after the fact. A React render
 * warning or an unhandled rejection is a real defect that a screenshot alone
 * will happily show you as a perfectly nice-looking page.
 */
export const test = base.extend<{
  consoleErrors: string[];
  fontsStubbed: boolean;
}>({
  /**
   * Keeps the suite off the network.
   *
   * `index.css` imports JetBrains Mono from Google Fonts at runtime, so every
   * page load fetched a stylesheet and a `.woff2` from a third-party CDN. That
   * makes each run a coin flip on someone else's uptime: gstatic answered 404
   * for one pinned font file and three unrelated scenarios failed the
   * console-error check, in a different combination on every re-run. The
   * request is answered with an empty stylesheet instead, so the type falls
   * back to the local monospace stack `tokens.css` already names.
   *
   * Safe for the screenshots because they are artifacts for a human to look at,
   * not baselines anything is compared against — no assertion here can see
   * which monospace face rendered.
   *
   * Automatic rather than opt-in: a test that forgot to ask for it would be the
   * one flaking, and the failure would point at the scenario rather than here.
   */
  fontsStubbed: [
    async ({ page }, use) => {
      await page.route(
        /^https:\/\/fonts\.(googleapis|gstatic)\.com\//,
        (route) =>
          route.fulfill({ status: 200, contentType: "text/css", body: "" }),
      );
      await use(true);
    },
    { auto: true },
  ],
  consoleErrors: async ({ page }, use) => {
    const errors: string[] = [];
    page.on("console", (message) => {
      if (message.type() === "error") errors.push(message.text());
    });
    page.on("pageerror", (error) => errors.push(`pageerror: ${error.message}`));
    await use(errors);
  },
});

export { expect };

/** Console noise that is expected and not a defect. */
const IGNORED_CONSOLE = [
  // The `boom` scenario and the ErrorBoundary tests log on purpose.
  "riabuild: render failed",
  "Fixture scenario `boom` throws on purpose",
  // React re-logs a caught error; the boundary rendering is the assertion.
  "The above error occurred in",
  "An error occurred in the <",
];

export type CheckOptions = {
  /** Screenshot name; omit to skip the screenshot. */
  screenshot?: string;
  /** Scenarios that intentionally render an error state. */
  expectConsoleErrors?: boolean;
  /** axe rules to disable, with a reason. Empty by default. */
  disableRules?: string[];
};

/**
 * Everything worth asserting about a rendered page that does not require a
 * human to look at it. The screenshot is for the part that does.
 */
export async function checkPage(
  page: Page,
  info: TestInfo,
  consoleErrors: string[],
  options: CheckOptions = {},
): Promise<void> {
  // Every screen this runs against draws the terminal frame, so its body is the
  // one thing whose presence means "React has mounted and rendered". A
  // web-first assertion rather than `networkidle`: the only third-party request
  // the app ever made was the Google Fonts stylesheet, which the `fontsStubbed`
  // fixture answers locally, so waiting for the network to fall quiet was
  // waiting on nothing and then guessing when nothing had finished.
  await expect(page.locator("main")).toBeVisible();
  // The blinking cursor animates forever; stop it so screenshots are stable.
  await page.addStyleTag({
    content: `*, *::before, *::after { animation: none !important; transition: none !important; }`,
  });

  await expectNoHorizontalOverflow(page);
  await expectNothingClipped(page);
  await expectAnchorTargetsExist(page);

  // Screenshot BEFORE the focus sweep. Focusing an element scrolls its
  // container to reveal it, so a screenshot taken afterwards shows tables
  // scrolled sideways with their first columns out of frame — a picture of the
  // test poking the page, not of the page.
  if (options.screenshot !== undefined) {
    await page.screenshot({
      path: `e2e/__screens__/${options.screenshot}-${info.project.name}.png`,
      fullPage: true,
    });
  }

  await expectVisibleFocus(page, info);

  const axe = new AxeBuilder({ page }).withTags([
    "wcag2a",
    "wcag2aa",
    "wcag21a",
    "wcag21aa",
  ]);
  if (options.disableRules !== undefined) {
    axe.disableRules(options.disableRules);
  }
  const results = await axe.analyze();
  expect(
    results.violations.map(
      (v) => `${v.id}: ${v.nodes.length} node(s) — ${v.help}`,
    ),
    "axe-core accessibility violations",
  ).toEqual([]);

  if (options.expectConsoleErrors !== true) {
    const unexpected = consoleErrors.filter(
      (message) => !IGNORED_CONSOLE.some((ignore) => message.includes(ignore)),
    );
    expect(unexpected, "unexpected console errors").toEqual([]);
  }
}

/** The document itself must never scroll sideways. */
async function expectNoHorizontalOverflow(page: Page): Promise<void> {
  const overflow = await page.evaluate(() => {
    const root = document.documentElement;
    if (root.scrollWidth <= root.clientWidth + 1) return null;
    // Name the widest offender so the failure says what to fix.
    const worst = [...document.querySelectorAll<HTMLElement>("body *")]
      .map((el) => ({
        right: el.getBoundingClientRect().right,
        tag: el.tagName.toLowerCase(),
        cls: el.className.toString().slice(0, 80),
        text: (el.textContent ?? "").trim().slice(0, 40),
      }))
      .filter((e) => e.right > root.clientWidth + 1)
      .sort((a, b) => b.right - a.right)[0];
    return {
      scrollWidth: root.scrollWidth,
      clientWidth: root.clientWidth,
      worst,
    };
  });
  expect(overflow, "the document scrolls horizontally").toBeNull();
}

/**
 * Text cut off by an ancestor that hides its overflow. A clipped label looks
 * like a design choice in a screenshot and like a bug to whoever needed to read
 * the rest of it.
 */
async function expectNothingClipped(page: Page): Promise<void> {
  const clipped = await page.evaluate(() => {
    const bad: string[] = [];
    for (const el of document.querySelectorAll<HTMLElement>("body *")) {
      const style = getComputedStyle(el);
      const hides =
        style.overflowX === "hidden" || style.overflowY === "hidden";
      if (!hides) continue;
      // Visually-hidden elements are 1x1 and clipped on purpose — that is the
      // whole technique. Flagging them would mean flagging every table caption.
      if (el.clientWidth <= 1 && el.clientHeight <= 1) continue;
      if (
        el.scrollWidth > el.clientWidth + 1 ||
        el.scrollHeight > el.clientHeight + 1
      ) {
        bad.push(
          `${el.tagName.toLowerCase()}.${el.className.toString().slice(0, 60)} ` +
            `[${el.scrollWidth}x${el.scrollHeight} in ${el.clientWidth}x${el.clientHeight}] ` +
            `"${(el.textContent ?? "").trim().slice(0, 40)}"`,
        );
      }
    }
    return bad;
  });
  expect(clipped, "content clipped by an overflow:hidden ancestor").toEqual([]);
}

/**
 * Every in-page link points at something that exists.
 *
 * This is the no-fake-affordances rule as an assertion. The tab strip is a row
 * of `#section` anchors, and a tab shown on a screen that has no such section —
 * the "not in the org" block, for one — is a control that silently does
 * nothing. Same broken promise as advertising a keybinding we never handle,
 * and just as invisible in a screenshot.
 */
async function expectAnchorTargetsExist(page: Page): Promise<void> {
  const dangling = await page.evaluate(() =>
    [...document.querySelectorAll<HTMLAnchorElement>('a[href^="#"]')]
      .map((a) => a.getAttribute("href") ?? "")
      .filter((href) => href.length > 1)
      .filter((href) => document.querySelector(href) === null),
  );
  expect(dangling, "in-page links whose target does not exist").toEqual([]);
}

/**
 * The page handles no keystrokes, so the browser's own tab order is the entire
 * keyboard story. Every interactive element must therefore be reachable by Tab
 * and must show where the focus went.
 *
 * Reached the way a keyboard user reaches it, which took two goes to get right.
 * The first version called `handle.focus()` and read
 * `getComputedStyle(el, ":focus-visible")` — and that assertion could not fail.
 * The second argument to `getComputedStyle` takes a *pseudo-element*, so Chrome
 * ignored `:focus-visible` and handed back the element's ordinary style; and
 * programmatic `focus()` does not put a button into `:focus-visible` in the
 * first place, so the rule under that selector was not in the style being read
 * either. Two mistakes that cancelled into a green check on a page that could
 * have had no focus ring at all.
 *
 * Tab is the fix for both. It sets the state for real, so the ordinary computed
 * style already carries the `:focus-visible` rule, and `:focus-visible` itself
 * becomes something worth asserting. It also walks the tab order rather than
 * the DOM order, which is the thing a keyboard user actually has.
 */
async function expectVisibleFocus(page: Page, info: TestInfo): Promise<void> {
  const selector =
    "a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled])";
  const reachable = await page.locator(selector).count();

  // Focus starts at the document, so the first Tab lands on the first element
  // in the browser's own order rather than continuing from wherever an earlier
  // assertion left it.
  await page.evaluate(() => {
    const active = document.activeElement;
    if (active instanceof HTMLElement) active.blur();
  });

  const LIMIT = 60;
  const invisible: string[] = [];
  let stopped = 0;
  for (let step = 0; step < LIMIT; step++) {
    await page.keyboard.press("Tab");
    const focused = await page.evaluate(() => {
      const el = document.activeElement;
      // Tabbing past the last control hands focus back to the document (and
      // then to the browser's own chrome, which is not ours to check).
      if (!(el instanceof HTMLElement) || el === document.body) return null;
      const style = getComputedStyle(el);
      const width = parseFloat(style.outlineWidth || "0");
      return {
        label: `${el.tagName.toLowerCase()} "${(el.textContent ?? "").trim().slice(0, 30)}"`,
        // The state the ring is written under. A control the browser refuses
        // to mark focus-visible on a keyboard tab is one whose ring nobody
        // will ever see, whatever the stylesheet says.
        focusVisible: el.matches(":focus-visible"),
        ringed:
          (style.outlineStyle !== "none" && width > 0) ||
          style.boxShadow !== "none",
      };
    });
    if (focused === null) break;
    stopped = step + 1;
    if (!focused.focusVisible) {
      invisible.push(`${focused.label} — not :focus-visible after Tab`);
    } else if (!focused.ringed) {
      invisible.push(`${focused.label} — :focus-visible with no outline`);
    }
  }

  if (stopped >= LIMIT) {
    info.annotations.push({
      type: "note",
      description: `focus check covered the first ${LIMIT} stops of ${reachable} interactive elements`,
    });
  }
  expect(stopped, "elements reached by Tab").toBeGreaterThan(0);
  expect(invisible, "interactive elements with no visible focus ring").toEqual(
    [],
  );
}
