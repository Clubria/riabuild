import { useData } from "../data/context";
import { Member } from "../data/types";
import { ErrorBoundary } from "../app/ErrorBoundary";
import { Profile } from "../components/Profile";
import { Sessions } from "../components/Sessions";
import { Install } from "../components/Install";
import { AuditLog, Members, OrgSettings } from "../components/LeadPanel";
import { Invite } from "../components/Invite";
import { IssuedKeys } from "../components/IssuedKeys";
import { SharedServers } from "../components/SharedServers";
import { Alert, Badge, Command, Panel, Tab } from "../ui";

/** What riabuild will do to the machine, stated before it does it. */
const MANIFEST: [string, string][] = [
  ["login", "sign this machine in to riabuild"],
  ["github_cli", "gh, authenticated, with read:org"],
  ["infisical_cli", "infisical, no token stored"],
  ["toolchain", "riabuild-owned Node and pnpm"],
  ["project", "the repo, cloned where you asked"],
  ["repo_status", "report drift — never pull for you"],
  ["claude_accounts", "Claude Code accounts of your own"],
  ["org_settings", "team policy, layered at launch"],
  ["env_local", "secrets per environment, brokered fresh each time"],
];

export function DASHBOARD_TABS(isLead: boolean): Tab[] {
  const tabs: Tab[] = [
    { id: "profile", label: "profile", href: "#profile" },
    { id: "install", label: "install", href: "#install" },
    { id: "machines", label: "machines", href: "#machines" },
  ];
  if (isLead) tabs.push({ id: "lead", label: "lead", href: "#lead" });
  return tabs;
}

export function Dashboard({ member }: { member: Member }) {
  const data = useData();
  const isLead = member.role === "lead";
  const config = data.orgConfig.state === "ready" ? data.orgConfig.value : null;

  if (data.membership.status === "not_member") {
    return (
      <div className="mx-auto max-w-xl py-4">
        <Alert
          tone="danger"
          title={`Not in the ${data.membership.org} org`}
        >
          <p className="wrap-value">
            Your GitHub account <strong>@{member.githubLogin}</strong> is not a
            member of {data.membership.org} yet. Ask your team lead for a GitHub
            invite — accepting it is all riabuild needs.
          </p>
        </Alert>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-7">
      <section>
        <p className="flex flex-wrap items-center gap-2 text-fg-faint">
          <span aria-hidden="true">$</span>
          <span>riabuild status</span>
        </p>
        <p className="mt-3 flex flex-wrap items-center gap-2">
          <span className="text-fg-dim">signed in as</span>
          <span className="min-w-0 text-fg wrap-value">
            @{member.githubLogin}
          </span>
          <Badge tone={member.role === "candidate" ? "muted" : "accent"}>
            {member.role}
          </Badge>
          {member.status === "suspended" && (
            <Badge tone="danger">suspended</Badge>
          )}
        </p>

        <h1 className="mt-4 text-xl leading-snug text-fg sm:text-2xl">
          One command builds the machine.
        </h1>
        <p className="mt-2 max-w-prose text-fg-dim">
          riabuild sets up everything below, checks each item is genuinely true
          of your laptop, and drops you into a shell with the team&rsquo;s
          environment. You choose nothing.
        </p>

        <ul className="mt-5 grid gap-x-6 gap-y-0.5 sm:grid-cols-2">
          {MANIFEST.map(([id, description]) => (
            <li key={id} className="flex items-baseline gap-2">
              <span aria-hidden="true" className="text-accent">
                ●
              </span>
              {/* `ch` is exact in a monospace face: 16ch clears the longest id
                  (`claude_accounts`) so every description starts on the same
                  column. Only from `sm` — at 380px that width would leave the
                  description nothing to wrap in. */}
              <span className="shrink-0 text-fg sm:min-w-[16ch]">{id}</span>
              <span className="min-w-0 text-fg-faint wrap-value">
                {description}
              </span>
            </li>
          ))}
        </ul>
      </section>

      {member.status === "suspended" && (
        <Alert tone="danger" title="Your account is suspended">
          <p>
            riabuild will refuse to hand this machine any secrets until a team
            lead reactivates you.
          </p>
        </Alert>
      )}

      {data.membership.status === "unavailable" && (
        <Alert tone="warn" title="GitHub check unavailable">
          <p>
            riabuild could not confirm your {data.membership.org} membership just
            now. You can still edit your profile; secrets will not be handed out
            until the check succeeds.
          </p>
        </Alert>
      )}

      <Panel id="profile" index="01" title="confirm your profile">
        <p className="mb-4 max-w-prose text-fg-dim">
          Prefilled from GitHub. Correct anything that is wrong.
        </p>
        <Profile member={member} />
      </Panel>

      <Panel id="install" index="02" title="install riabuild">
        <p className="mb-3 max-w-prose text-fg-dim">
          One package, from the Clubria repository for your platform. riabuild
          keeps itself current from there.
        </p>
        <Install />
      </Panel>

      <Panel index="03" title="run it">
        <Command command="riabuild" />
        <p className="mt-3 max-w-prose text-fg-dim">
          The first run signs this machine in through your browser and comes
          straight back to the terminal. Every run after that checks the machine
          and repairs what drifted. Type <span className="text-fg">exit</span> to
          leave the environment.
        </p>
        {config !== null && (
          <p className="mt-3 text-xs text-fg-faint wrap-value">
            default repo {config.repoSlug} · needs riabuild v
            {config.minCliVersion} or newer
          </p>
        )}
      </Panel>

      <Panel id="machines" index="04" title="your machines">
        <Sessions />
      </Panel>

      {isLead && (
        <>
          <Panel id="lead" index="lead" title="invite ahead of time" tone="accent">
            <ErrorBoundary label="the invite panel">
              <Invite />
            </ErrorBoundary>
          </Panel>
          <Panel index="lead" title="members and roles" tone="accent">
            <ErrorBoundary label="the member list">
              <Members viewerId={member._id} />
            </ErrorBoundary>
          </Panel>
          <Panel index="lead" title="org configuration" tone="accent">
            <ErrorBoundary label="org configuration">
              <OrgSettings />
            </ErrorBoundary>
          </Panel>
          <Panel index="lead" title="the team's servers" tone="accent">
            <ErrorBoundary label="the team's servers">
              <SharedServers />
            </ErrorBoundary>
          </Panel>
          <Panel index="lead" title="issued SSH keys" tone="accent">
            <ErrorBoundary label="the issued keys">
              <IssuedKeys />
            </ErrorBoundary>
          </Panel>
          <Panel index="lead" title="audit log" tone="accent">
            <ErrorBoundary label="the audit log">
              <AuditLog />
            </ErrorBoundary>
          </Panel>
        </>
      )}
    </div>
  );
}
