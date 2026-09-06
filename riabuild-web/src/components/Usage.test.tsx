// @vitest-environment jsdom
import { render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import { DataContext } from "../data/context";
import { SCENARIOS } from "../dev/scenarios";
import { Usage } from "./Usage";

/**
 * The things about this panel a screenshot cannot assert: that a row is a
 * Claude account rather than a developer, that an account riabuild cannot name
 * is still a row, that a missing rate-limit window is not drawn as zero, and
 * that the bar is decoration a screen reader skips rather than eight announced
 * block characters.
 */

function renderUsage(data: ReturnType<typeof SCENARIOS.lead>) {
  render(
    <DataContext.Provider value={data}>
      <Usage />
    </DataContext.Provider>,
  );
}

describe("Usage", () => {
  /**
   * The keying rule from the design, as a test. A rate-limit window belongs to
   * the Anthropic account it was spent from, so the row names that account —
   * and a developer's GitHub login, which names a person and not a window, is
   * nowhere in the table.
   */
  test("a row is a Claude account, not a developer", () => {
    const data = SCENARIOS.lead();
    if (data.usage.state !== "ready") throw new Error("fixture invariant");

    renderUsage(data);

    expect(
      screen.getByRole("columnheader", { name: /claude account/i }),
    ).toBeInTheDocument();
    expect(screen.getByText("ada@clubria.com")).toBeInTheDocument();

    for (const header of screen.getAllByRole("columnheader")) {
      expect(header.textContent ?? "").not.toMatch(/github|member|developer/i);
    }
    // One person's two sign-ins are two rows: merging them would report a
    // window neither account has.
    expect(screen.getByText("ada@personal.example")).toBeInTheDocument();
  });

  /**
   * The fields removed on 2026-09-06, kept out by name. Cost is list price
   * against subscriptions nobody pays per token with — a number that reads as
   * money and is not — and the line counts and the reset countdown were three
   * columns nobody used pushing the four that matter off a narrow screen.
   */
  test("no cost, lines or reset column comes back", () => {
    renderUsage(SCENARIOS.lead());

    for (const header of screen.getAllByRole("columnheader")) {
      expect(header.textContent ?? "").not.toMatch(
        /list-price|spend|cost|\$|lines|resets/i,
      );
    }
    expect(screen.queryByText(/list-price equivalent/i)).toBeNull();
  });

  test("rate-limit headroom comes before the session count", () => {
    renderUsage(SCENARIOS.lead());

    const headers = screen
      .getAllByRole("columnheader")
      .map((cell) => cell.textContent?.trim() ?? "");
    expect(headers.indexOf("5h used")).toBeLessThan(
      headers.indexOf("total sessions"),
    );
    expect(headers.indexOf("7d used")).toBeLessThan(
      headers.indexOf("total sessions"),
    );
  });

  /**
   * An account riabuild has no email for is still an account somebody is
   * spending a window from. Dropping the row would lose the usage; borrowing a
   * developer's login for it would assert a link riabuild never observed.
   */
  test("an account with no email is a row that says so", () => {
    const data = SCENARIOS.lead();
    if (data.usage.state !== "ready") throw new Error("fixture invariant");
    const unnamed = data.usage.value.rows.filter(
      (row) => row.accountEmail === null,
    );
    expect(unnamed.length).toBeGreaterThan(0);

    renderUsage(data);

    expect(screen.getAllByText(/unnamed account/i).length).toBe(unnamed.length);
    // Enough of the config-directory uuid to tell two of them apart.
    expect(
      screen.getByText((unnamed[0].accountId ?? "").slice(0, 8)),
    ).toBeInTheDocument();
  });

  /**
   * An API-key or Console login reports no `rate_limits` block at all, which is
   * not the same claim as a window sitting at zero. Drawing it as `0%` would
   * report somebody as having their whole allowance left.
   */
  test("an account with no rate-limit window is a dash, not zero", () => {
    const data = SCENARIOS.lead();
    if (data.usage.state !== "ready") throw new Error("fixture invariant");
    const rowsWithout = data.usage.value.rows.filter(
      (row) => row.fiveHourPct === null,
    );
    expect(rowsWithout.length).toBeGreaterThan(0);

    renderUsage(data);

    for (const row of rowsWithout) {
      const cells = screen
        .getByText(row.accountEmail ?? /unnamed account/i)
        .closest("tr")
        ?.querySelectorAll("td");
      if (cells === undefined) throw new Error("the row renders as a row");
      // Columns are account, 5h, 7d, … — the two meters sit at 1 and 2.
      expect(cells[1].textContent).toBe("—");
      expect(cells[2].textContent).toBe("—");
      expect(cells[1].textContent).not.toContain("0%");
    }
  });

  /**
   * The bar is the status line's own `█`/`░` vocabulary and carries no
   * information the percentage beside it does not. Announcing it would read as
   * "full block" eight times per row.
   */
  test("the meter blocks are hidden from assistive technology", () => {
    renderUsage(SCENARIOS.lead());

    const blocks = document.querySelectorAll("[aria-hidden='true']");
    const bars = Array.from(blocks).filter((el) =>
      /[█░]/.test(el.textContent ?? ""),
    );
    expect(bars.length).toBeGreaterThan(0);

    // And the number is still there to be read, with a name saying which
    // window it belongs to.
    expect(
      screen.getAllByText(/five-hour window used/i).length,
    ).toBeGreaterThan(0);
    expect(
      screen.getAllByText(/seven-day window used/i).length,
    ).toBeGreaterThan(0);
  });

  /** A floor announced as a floor. */
  test("a truncated row says so", () => {
    const data = SCENARIOS.overflow();
    if (data.usage.state !== "ready") throw new Error("fixture invariant");
    expect(data.usage.value.rows.some((row) => row.truncated)).toBe(true);

    renderUsage(data);

    expect(screen.getAllByText("partial").length).toBeGreaterThan(0);
  });

  test("nothing reported yet explains why, rather than reading as broken", () => {
    renderUsage(SCENARIOS["usage-empty"]());

    expect(screen.getByText(/Nothing reported yet/i)).toBeInTheDocument();
    // There is nothing for a lead to switch on any more, so the empty state has
    // to name what it is waiting for instead.
    expect(screen.getByText(/about once a minute/i)).toBeInTheDocument();
    // The empty state is the table's stand-in, so there is no table to read.
    expect(screen.queryByRole("table")).toBeNull();
  });

  test("a failed query is an alert, not an empty table", () => {
    renderUsage(SCENARIOS["usage-error"]());

    expect(screen.getByText(/Could not load usage/i)).toBeInTheDocument();
    expect(screen.queryByRole("table")).toBeNull();
  });
});
