// @vitest-environment jsdom
import { render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import { DataContext } from "../data/context";
import { SCENARIOS } from "../dev/scenarios";
import { Usage } from "./Usage";

/**
 * The three things about this panel a screenshot cannot assert: that the cost
 * column is labelled, that a missing rate-limit window is not drawn as zero,
 * and that the bar is decoration a screen reader skips rather than eight
 * announced block characters.
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
   * The labelling rule from the design, as a test. These are personal Pro and
   * Max subscriptions: the dollar figure is what the work would have cost
   * against the public price sheet, and an unlabelled one ends up in a budget.
   */
  test("cost is labelled list-price equivalent, and never called spend", () => {
    renderUsage(SCENARIOS.lead());

    expect(
      screen.getByRole("columnheader", { name: /list-price equiv/i }),
    ).toBeInTheDocument();
    expect(screen.getByText(/list-price equivalent/i)).toBeInTheDocument();
    expect(screen.getByText(/not money anyone spent/i)).toBeInTheDocument();

    // No heading calls it spend, or cost, or anything else that reads as money
    // somebody paid. The word appears once in the footnote, in the sentence
    // saying it is not one — which is the labelling, not a leak of it.
    for (const header of screen.getAllByRole("columnheader")) {
      expect(header.textContent ?? "").not.toMatch(/spend|^cost$|\$/i);
    }
  });

  test("rate-limit headroom comes before sessions and cost", () => {
    renderUsage(SCENARIOS.lead());

    const headers = screen
      .getAllByRole("columnheader")
      .map((cell) => cell.textContent?.trim() ?? "");
    expect(headers.indexOf("5h used")).toBeLessThan(
      headers.indexOf("sessions"),
    );
    expect(headers.indexOf("7d used")).toBeLessThan(
      headers.indexOf("sessions"),
    );
    expect(headers.indexOf("sessions")).toBeLessThan(
      headers.indexOf("list-price equiv"),
    );
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
        .getByText(`@${row.githubLogin}`)
        .closest("tr")
        ?.querySelectorAll("td");
      if (cells === undefined) throw new Error("the row renders as a row");
      // Columns are github, 5h, 7d, … — the two meters sit at 1 and 2.
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
