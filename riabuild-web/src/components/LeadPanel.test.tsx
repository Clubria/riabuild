// @vitest-environment jsdom
import { render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import { DataContext } from "../data/context";
import { SCENARIOS } from "../dev/scenarios";
import userEvent from "@testing-library/user-event";
import { Members, OrgSettings } from "./LeadPanel";
// The status line the CLI installs, read from the one place a browser bundle
// can still reach: `LeadPanel` keeps its own copy because it cannot import a
// Convex server module, and this is what stops the two drifting.
import { DEFAULT_STATUS_LINE } from "../../convex/org";

describe("Members", () => {
  test("a lead sees a member id column, and it drops on a narrow viewport", () => {
    const data = SCENARIOS.lead();
    const viewer = data.viewer.state === "ready" ? data.viewer.value : null;
    if (viewer === null)
      throw new Error("the lead fixture always has a viewer");

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

function renderSettings(data: ReturnType<typeof SCENARIOS.lead>) {
  render(
    <DataContext.Provider value={data}>
      <OrgSettings />
    </DataContext.Provider>,
  );
}

/** A lead fixture whose `updateOrg` records what the form sent. */
function recordingLead() {
  const calls: Record<string, unknown>[] = [];
  return {
    calls,
    data: {
      ...SCENARIOS.lead(),
      updateOrg: async (p: Record<string, unknown>) => {
        calls.push(p);
      },
    },
  };
}

function settingsBox() {
  return screen.getByLabelText<HTMLTextAreaElement>(/claude code settings/i);
}

function sentSettings(payload: Record<string, unknown>): ClaudeSettings {
  return JSON.parse(payload.claudeSettings as string) as ClaudeSettings;
}

type ClaudeSettings = Record<string, unknown> & {
  statusLine?: { type: string; command: string };
};

describe("OrgSettings — the status line", () => {
  /**
   * The whole point of the read-only line. Until it existed the status line
   * was inside the free-text settings box, so a lead could save a command the
   * CLI now hard-refuses — and the first people to find out were every
   * developer in the org, on their next run.
   */
  test("the command is shown, and there is no box to type another one in", () => {
    renderSettings(SCENARIOS.lead());

    expect(screen.getByText(DEFAULT_STATUS_LINE.command)).toBeInTheDocument();
    // Read-only means read-only: not a disabled input, not a field at all.
    expect(screen.queryByLabelText(/status line/i)).not.toBeInTheDocument();
    expect(settingsBox().value).not.toContain("statusLine");
  });

  test("a status line already stored is taken out of the box a lead edits", () => {
    // This is what an org that saved one before the gate existed holds. The
    // lead never sees it, cannot retype it, and loses it the moment they save.
    const lead = SCENARIOS.lead();
    if (lead.orgConfig.state !== "ready") throw new Error("fixture invariant");
    renderSettings({
      ...lead,
      orgConfig: {
        ...lead.orgConfig,
        value: {
          ...lead.orgConfig.value,
          claudeSettings: JSON.stringify({
            model: "claude-opus-5",
            statusLine: { type: "command", command: "node /tmp/theirs.js" },
          }),
        },
      },
    });

    expect(settingsBox().value).not.toContain("/tmp/theirs.js");
    expect(settingsBox().value).toContain("claude-opus-5");
  });

  test("saving puts riabuild's own status line back", async () => {
    const { calls, data } = recordingLead();
    renderSettings(data);

    await userEvent.click(
      screen.getByRole("button", { name: /save org config/i }),
    );
    // Pinned against `convex/org.ts` rather than a literal: `org.update`
    // refuses every other value, so a copy that drifted would refuse the save.
    expect(sentSettings(calls[0]).statusLine).toEqual(DEFAULT_STATUS_LINE);
  });

  test("a non-conforming status line is replaced by saving, not preserved", async () => {
    // The recovery path for an org that already stored one: open the settings
    // screen, press save. No CLI release, no hand-edited row.
    const lead = SCENARIOS.lead();
    if (lead.orgConfig.state !== "ready") throw new Error("fixture invariant");
    const { calls, data } = recordingLead();
    renderSettings({
      ...data,
      orgConfig: {
        ...lead.orgConfig,
        value: {
          ...lead.orgConfig.value,
          claudeSettings: JSON.stringify({
            statusLine: { type: "command", command: "node /tmp/theirs.js" },
          }),
        },
      },
    });

    await userEvent.click(
      screen.getByRole("button", { name: /save org config/i }),
    );
    expect(sentSettings(calls[0]).statusLine).toEqual(DEFAULT_STATUS_LINE);
  });

  test("settings that do not parse are sent as they stand", async () => {
    // One "must be valid JSON" message, from the server, rather than a second
    // one here that would disagree with it eventually.
    const { calls, data } = recordingLead();
    renderSettings(data);

    const box = screen.getByLabelText(/claude code settings/i);
    await userEvent.clear(box);
    await userEvent.type(box, "{{ not json");
    await userEvent.click(
      screen.getByRole("button", { name: /save org config/i }),
    );
    expect(calls[0].claudeSettings).toBe("{ not json");
  });
});

describe("OrgSettings — the team's ngrok authtoken", () => {
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

  test("removing the token asks first, and a slipped click destroys nothing", async () => {
    // The one irreversible control on the page: no route returns the token, so
    // a second `updateOrg` from a double-click used to wipe a secret nothing
    // could give back. The first click may only arm the second.
    const calls: Record<string, unknown>[] = [];
    const data = {
      ...SCENARIOS.lead(),
      updateOrg: async (p: Record<string, unknown>) => {
        calls.push(p);
      },
    };
    renderSettings(data);

    const arm = screen.getByRole("button", { name: /remove ngrok authtoken/i });
    await userEvent.dblClick(arm);
    expect(calls).toEqual([]);

    // And it names what is about to go, rather than asking "are you sure".
    expect(
      screen.getByText(/Remove the ngrok authtoken ending …tok3\?/),
    ).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: /keep it/i }));
    expect(calls).toEqual([]);
    expect(
      screen.queryByText(/Remove the ngrok authtoken ending/),
    ).not.toBeInTheDocument();
  });

  test("confirming the removal is what sends it", async () => {
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
    await userEvent.click(
      screen.getByRole("button", { name: /^remove the token$/i }),
    );
    expect(calls).toHaveLength(1);
    expect(calls[0].ngrokAuthToken).toBe("");
  });

  test("marking secrets rotated says nothing else with it", async () => {
    // It shares a mutation with the settings save and must not carry any of
    // that form's fields along — least of all a blank ngrok token.
    const calls: Record<string, unknown>[] = [];
    const data = {
      ...SCENARIOS.lead(),
      updateOrg: async (p: Record<string, unknown>) => {
        calls.push(p);
      },
    };
    renderSettings(data);

    await userEvent.click(
      screen.getByRole("button", { name: /mark secrets rotated/i }),
    );
    expect(calls).toEqual([{ markSecretsRotated: true }]);
  });
});
