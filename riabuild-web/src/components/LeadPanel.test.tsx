// @vitest-environment jsdom
import { render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import { DataContext } from "../data/context";
import { SCENARIOS } from "../dev/scenarios";
import userEvent from "@testing-library/user-event";
import { Members, OrgSettings } from "./LeadPanel";

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

describe("OrgSettings — the team's ngrok authtoken", () => {
  function renderSettings(data: ReturnType<typeof SCENARIOS.lead>) {
    render(
      <DataContext.Provider value={data}>
        <OrgSettings />
      </DataContext.Provider>,
    );
  }

  test("a token that is set is shown as a hint and never as itself", () => {
    // The value is deliberately unrecoverable from this screen: a lead needs to
    // recognise the token they pasted, never to read it back.
    const data = SCENARIOS.lead();
    renderSettings(data);

    const field = screen.getByLabelText(/ngrok authtoken/i);
    expect(field).toHaveValue("");
    expect(screen.getByText(/…tok3/)).toBeInTheDocument();
  });

  test("a team with no token is told what that costs them", () => {
    const data = SCENARIOS["ngrok-unset"]();
    renderSettings(data);

    expect(screen.getByText(/unauthenticated/i)).toBeInTheDocument();
    // Nothing to remove, so nothing offers to.
    expect(
      screen.queryByRole("button", { name: /remove ngrok authtoken/i }),
    ).not.toBeInTheDocument();
  });

  test("saving without touching the field leaves the token alone", async () => {
    // The field is blank because it is write-only, not because the lead cleared
    // it — sending that blank would wipe the team's token on every unrelated
    // settings save.
    const calls: unknown[] = [];
    const data = {
      ...SCENARIOS.lead(),
      updateOrg: async (p: unknown) => {
        calls.push(p);
      },
    };
    renderSettings(data);

    await userEvent.click(
      screen.getByRole("button", { name: /save org config/i }),
    );
    expect(calls).toHaveLength(1);
    expect(Object.keys(calls[0] as object)).not.toContain("ngrokAuthToken");
  });

  test("a token typed into the field is saved with the rest", async () => {
    const calls: Record<string, unknown>[] = [];
    const data = {
      ...SCENARIOS.lead(),
      updateOrg: async (p: Record<string, unknown>) => {
        calls.push(p);
      },
    };
    renderSettings(data);

    await userEvent.type(
      screen.getByLabelText(/ngrok authtoken/i),
      "2newTOKEN_value",
    );
    await userEvent.click(
      screen.getByRole("button", { name: /save org config/i }),
    );
    expect(calls[0].ngrokAuthToken).toBe("2newTOKEN_value");
  });

  test("removing the token is its own deliberate action", async () => {
    const calls: Record<string, unknown>[] = [];
    const data = {
      ...SCENARIOS.lead(),
      updateOrg: async (p: Record<string, unknown>) => {
        calls.push(p);
      },
    };
    renderSettings(data);

    await userEvent.click(
      screen.getByRole("button", { name: /remove ngrok authtoken/i }),
    );
    expect(calls[0].ngrokAuthToken).toBe("");
  });
});
