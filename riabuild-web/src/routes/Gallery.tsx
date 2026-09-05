import { useState } from "react";
import {
  Alert,
  Badge,
  Button,
  Column,
  Command,
  Copyable,
  DataTable,
  Dot,
  Empty,
  Field,
  KeyValue,
  Loading,
  Panel,
  Select,
  TextArea,
  Tone,
} from "../ui";
import { SCENARIO_NAMES } from "../dev/scenarios";
import { Install } from "../components/Install";
import { Platform } from "../lib/platform";

const TONES: Tone[] = ["default", "accent", "ok", "warn", "danger", "muted"];

/**
 * Every component, in every state, on one page.
 *
 * Pages exercise components in the combinations the product happens to need.
 * This exercises them in the combinations they claim to support, which is where
 * the gaps are — a tone nobody uses yet, a button that is pending and disabled,
 * a table with one column and no actions.
 *
 * Dev builds only; `route()` resolves `/__ui` to a 404 in production.
 */
export function Gallery() {
  return (
    <div className="flex flex-col gap-8">
      <p className="text-fg-dim">
        Component gallery. Dev builds only — this path 404s in production.
      </p>

      <Section name="Button">
        <Row label="variants">
          <Button variant="primary">primary</Button>
          <Button variant="quiet">quiet</Button>
          <Button variant="danger">danger</Button>
        </Row>
        <Row label="disabled">
          <Button variant="primary" disabled>
            primary
          </Button>
          <Button variant="quiet" disabled>
            quiet
          </Button>
          <Button variant="danger" disabled>
            danger
          </Button>
        </Row>
        <Row label="pending">
          <Button variant="primary" pending pendingLabel="saving">
            save
          </Button>
          <Button variant="quiet" pending>
            no pending label
          </Button>
        </Row>
        {/* A toggle rather than an action. The pair is the point: the two
            states have to be told apart at a glance and by a screen reader, and
            it was the second that was missing until `pressed` existed. */}
        <Row label="pressed">
          <Button variant="primary" pressed>
            on
          </Button>
          <Button variant="quiet" pressed={false}>
            off
          </Button>
        </Row>
        <Row label="as link">
          <Button variant="quiet" href="/__ui">
            href
          </Button>
        </Row>
        <Row label="long label">
          {/* Wraps rather than pushing the page sideways at 380px. Real labels
              are two words; this is the hostile case. */}
          <Button variant="primary">
            {"a-very-long-button-label-that-must-wrap-not-overflow"}
          </Button>
        </Row>
      </Section>

      <Section name="Badge">
        <Row label="tones">
          {TONES.map((tone) => (
            <Badge key={tone} tone={tone}>
              {tone}
            </Badge>
          ))}
        </Row>
        <Row label="long content">
          <Badge tone="danger">{"suspended-".repeat(6)}</Badge>
        </Row>
      </Section>

      <Section name="Dot">
        <Row label="tones">
          {TONES.map((tone) => (
            <Dot key={tone} tone={tone} label={tone} />
          ))}
        </Row>
      </Section>

      <Section name="Alert">
        {TONES.map((tone) => (
          <Alert key={tone} tone={tone} title={`tone: ${tone}`}>
            <p>A sentence explaining what the reader should do about it.</p>
          </Alert>
        ))}
        <Alert tone="warn" title="No body" />
        <Alert tone="danger" title={"unbroken-".repeat(30)}>
          <p className="wrap-value">{"x".repeat(300)}</p>
        </Alert>
      </Section>

      <Section name="Panel">
        <Panel title="plain">body</Panel>
        <Panel title="with index" index="01">
          body
        </Panel>
        <Panel
          title="with subtitle and actions"
          index="02"
          subtitle="A line of explanation above the body."
          actions={<Button variant="quiet">action</Button>}
        >
          body
        </Panel>
        {TONES.map((tone) => (
          <Panel key={tone} title={`tone: ${tone}`} tone={tone} dense>
            dense body
          </Panel>
        ))}
        <Panel title={"a-very-long-panel-title-".repeat(4)}>
          A title longer than the panel is wide.
        </Panel>
        <Panel>Untitled panel — no notch in the rule.</Panel>
      </Section>

      <Section name="Loading / Empty">
        <Loading />
        <Loading label="loading something specific" />
        <Empty title="Nothing here yet." />
        <Empty glyph="⌁" title="With a body and an action.">
          An explanation of what would put something here.
        </Empty>
        <Empty
          glyph="⌂"
          title="With an action"
          action={<Button variant="quiet">do the thing</Button>}
        >
          Body text.
        </Empty>
      </Section>

      <Section name="Command">
        <Command command="riabuild" />
        <Command command="brew install clubria/tap/riabuild" />
        <Command command={`riabuild --flag ${"x".repeat(200)}`} />
        <Command command={"line one\nline two\nline three"} prompt=">" />
      </Section>

      <Section name="Copyable">
        <Row label="uuid">
          <Copyable
            value="550e8400-e29b-41d4-a716-446655440000"
            label="member id"
          />
        </Row>
        <Row label="no dashes">
          <Copyable value="nodashesatall" label="value" />
        </Row>
        <Row label="long, no dashes">
          {/* The hostile case: no dash to truncate at, and long enough that
              an unbounded prefix would overflow the row. */}
          <Copyable value={"unbrokentoken".repeat(6)} label="long value" />
        </Row>
        <Row label="empty">
          <Copyable value="" label="empty value" />
        </Row>
      </Section>

      {/* All three, forced, because the live panel only ever shows the one it
          guessed — and apt is the widest thing the console renders. */}
      <Section name="Install">
        {(["macos", "apt", "dnf"] as Platform[]).map((platform) => (
          <Install key={platform} platform={platform} />
        ))}
      </Section>

      <Section name="KeyValue">
        <KeyValue
          rows={[
            { label: "device", value: "dana-mbp-16" },
            { label: "riabuild", value: "v2026.08.04", tone: "accent" },
            { label: "asked", value: "just now" },
            { label: "empty", value: "" },
            { label: "long".repeat(6), value: "y".repeat(200) },
          ]}
        />
      </Section>

      <Section name="Field / TextArea / Select">
        <GalleryForm />
      </Section>

      <Section name="DataTable">
        <DataTable
          caption="Gallery table, populated"
          columns={TABLE_COLUMNS}
          rows={TABLE_ROWS}
          rowKey={(r) => r.id}
          renderActions={(r) => (
            <Button variant="danger" aria-label={`remove ${r.name}`}>
              remove
            </Button>
          )}
          empty={<Empty title="unused" />}
        />
        <DataTable
          caption="Gallery table, no actions"
          columns={TABLE_COLUMNS}
          rows={TABLE_ROWS.slice(0, 1)}
          rowKey={(r) => r.id}
          empty={<Empty title="unused" />}
        />
        {/* Prose under a row, spanning every column — and the row beside it
            with none, because a table where every row has a second line and one
            where only some do are two different pictures. The unbroken
            120-character name is what makes this state worth a slot: it is what
            squeezes a `grow` column to its floor, which is the reason a
            description cannot live in one. */}
        <DataTable
          caption="Gallery table, rows with a line under them"
          columns={TABLE_COLUMNS}
          rows={TABLE_ROWS}
          rowKey={(r) => r.id}
          renderSubRow={(r) =>
            r.detail === "" ? null : (
              <span className="text-fg-faint">
                {r.detail}, said at length under the row it is about rather than
                inside a column that has no room for a sentence.
              </span>
            )
          }
          renderActions={(r) => (
            <Button variant="quiet" aria-label={`edit ${r.name}`}>
              edit
            </Button>
          )}
          empty={<Empty title="unused" />}
        />
        <DataTable
          caption="Gallery table, empty"
          columns={TABLE_COLUMNS}
          rows={[]}
          rowKey={(r) => r.id}
          empty={<Empty title="No rows.">This is the empty slot.</Empty>}
        />
      </Section>

      <Section name="Scenarios">
        <ul className="grid gap-0.5 sm:grid-cols-3">
          {SCENARIO_NAMES.map((name) => (
            <li key={name}>
              <a
                className="text-accent underline-offset-2 hover:underline"
                href={`/?scenario=${encodeURIComponent(name)}`}
              >
                {name}
              </a>
            </li>
          ))}
        </ul>
      </Section>
    </div>
  );
}

type GalleryRow = { id: string; name: string; state: string; detail: string };

const TABLE_ROWS: GalleryRow[] = [
  { id: "1", name: "short", state: "active", detail: "a detail" },
  {
    id: "2",
    name: "unbroken-" + "z".repeat(120),
    state: "revoked",
    detail: "another",
  },
  { id: "3", name: "田中さん 🚀", state: "expired", detail: "" },
];

const TABLE_COLUMNS: Column<GalleryRow>[] = [
  { key: "name", header: "name", grow: true, render: (r) => r.name },
  {
    key: "state",
    header: "state",
    render: (r) => (
      <Badge tone={r.state === "active" ? "ok" : "muted"}>{r.state}</Badge>
    ),
  },
  {
    key: "detail",
    header: "detail (wide only)",
    priority: "wide",
    render: (r) => r.detail || "—",
  },
  { key: "num", header: "n", align: "end", render: (r) => r.id },
];

function GalleryForm() {
  const [text, setText] = useState("a value");
  const [area, setArea] = useState('{\n  "key": "value"\n}');
  const [choice, setChoice] = useState("developer");
  const options = [
    { value: "candidate", label: "candidate" },
    { value: "developer", label: "developer" },
    { value: "lead", label: "lead" },
  ];

  return (
    <div className="grid max-w-2xl gap-4">
      <Field label="plain" value={text} onChange={setText} />
      <Field
        label="with hint"
        value={text}
        onChange={setText}
        hint="A line of guidance under the control."
      />
      <Field
        label="with error"
        value="not-an-email"
        onChange={() => {}}
        error="That does not look like an email address."
      />
      <Field
        label="disabled"
        value="cannot edit"
        onChange={() => {}}
        disabled
      />
      <Field
        label="empty with placeholder"
        value=""
        onChange={() => {}}
        placeholder="2026.08.04"
      />
      <Field
        label="overlong value"
        value={"q".repeat(300)}
        onChange={() => {}}
      />
      <TextArea label="textarea" value={area} onChange={setArea} rows={6} />
      <TextArea
        label="textarea with error"
        value="{ not json"
        onChange={() => {}}
        rows={3}
        error="claudeSettings must be valid JSON."
      />
      <Select
        label="select"
        value={choice}
        options={options}
        onChange={setChoice}
      />
      <div>
        <span className="mb-1 block text-xs tracking-wider text-fg-dim uppercase">
          compact select (label hidden, kept for screen readers)
        </span>
        <Select
          compact
          label="Role"
          value={choice}
          options={options}
          onChange={setChoice}
        />
      </div>
    </div>
  );
}

function Section({
  name,
  children,
}: {
  name: string;
  children: React.ReactNode;
}) {
  return (
    <section>
      <h2 className="mb-3 border-b border-rule pb-1 text-xs tracking-widest text-accent uppercase">
        {name}
      </h2>
      <div className="flex flex-col gap-4">{children}</div>
    </section>
  );
}

function Row({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-wrap items-center gap-3">
      <span className="w-28 shrink-0 text-xs text-fg-faint">{label}</span>
      <span className="flex min-w-0 flex-wrap items-center gap-2">
        {children}
      </span>
    </div>
  );
}
