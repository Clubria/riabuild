// @vitest-environment jsdom
import { render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import { DataContext } from "../data/context";
import { SCENARIOS } from "../dev/scenarios";
import { Members } from "./LeadPanel";

describe("Members", () => {
  test("a lead sees a member id column, and it drops on a narrow viewport", () => {
    const data = SCENARIOS.lead();
    const viewer = data.viewer.state === "ready" ? data.viewer.value : null;
    if (viewer === null) throw new Error("the lead fixture always has a viewer");

    render(
      <DataContext.Provider value={data}>
        <Members viewerId={viewer._id} />
      </DataContext.Provider>,
    );

    const header = screen.getByRole("columnheader", { name: /member id/i });
    expect(header).toBeInTheDocument();
    // `priority: "wide"` is what DataTable reads to hide a column below
    // 640px — assert the same class the visual suite relies on at 380px.
    expect(header.className).toMatch(/hidden/);
    expect(header.className).toMatch(/sm:table-cell/);

    // Not just the header: the actual id for a real row, so this test fails
    // if the column stops reading `m.memberId` (or stops rendering at all).
    if (data.members.state !== "ready") throw new Error("fixture invariant");
    for (const member of data.members.value) {
      expect(screen.getByText(member.memberId)).toBeInTheDocument();
    }
  });
});
