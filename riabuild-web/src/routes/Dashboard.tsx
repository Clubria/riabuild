import { useQuery } from "convex/react";
import { api } from "../../convex/_generated/api";
import { Chip, CommandLine, Notice, Step } from "../components/primitives";
import { Profile } from "../components/Profile";
import { Sessions } from "../components/Sessions";
import { AuditLog, Members, OrgSettings } from "../components/LeadPanel";
import { OrgMembership } from "../useOrgMembership";

/** What riabuild will do to the machine, stated before it does it. */
const MANIFEST: [string, string][] = [
  ["login", "sign this machine in to riabuild"],
  ["github_cli", "gh, authenticated, with read:org"],
  ["infisical_cli", "infisical, no token stored"],
  ["toolchain", "riabuild-owned Node and pnpm"],
  ["project", "the repo, cloned where you asked"],
  ["repo_status", "report drift — never pull for you"],
  ["claude_profiles", "a Claude Code profile of your own"],
  ["org_settings", "team policy, layered at launch"],
  ["env_local", "secrets, brokered fresh each time"],
];

export function Dashboard({
  member,
  membership,
}: {
  member: {
    _id: string;
    githubLogin: string;
    firstName: string;
    lastName: string;
    email: string;
    role: "candidate" | "developer" | "lead";
    status: "active" | "suspended";
  };
  membership: OrgMembership;
}) {
  const config = useQuery(api.org.get);
  const isLead = member.role === "lead";

  if (membership.status === "not_member") {
    return (
      <div className="max-w-xl py-10">
        <Notice tone="signal" title={`Not in the ${membership.org} org`}>
          <p className="mt-2">
            Your GitHub account{" "}
            <span className="mono">@{member.githubLogin}</span> is not a member
            of {membership.org} yet. Ask your team lead for a GitHub invite —
            accepting it is all riabuild needs.
          </p>
        </Notice>
      </div>
    );
  }

  return (
    <>
      <section className="step-in py-10">
        <p className="eyebrow mb-4">
          Signed in as @{member.githubLogin} ·{" "}
          <Chip tone={member.role === "candidate" ? "muted" : "ink"}>
            {member.role}
          </Chip>
          {member.status === "suspended" && (
            <>
              {" "}
              <Chip tone="signal">suspended</Chip>
            </>
          )}
        </p>
        <h1 className="display text-4xl sm:text-6xl">
          One command
          <br />
          builds the machine.
        </h1>
        <p className="mt-5 max-w-xl">
          riabuild sets up everything below, checks each item is genuinely true
          of your laptop, and drops you into a shell with the team&rsquo;s
          environment. You choose nothing.
        </p>

        <ol className="mt-8 grid gap-x-8 gap-y-1 sm:grid-cols-2">
          {MANIFEST.map(([id, description]) => (
            <li key={id} className="flex items-baseline gap-3">
              <span className="mono text-ink" aria-hidden="true">
                ●
              </span>
              <span className="mono text-graphite">{id}</span>
              <span className="text-muted">{description}</span>
            </li>
          ))}
        </ol>
      </section>

      {membership.status === "unavailable" && (
        <div className="mb-6">
          <Notice tone="signal" title="GitHub check unavailable">
            <p>
              riabuild could not confirm your {membership.org} membership just
              now. You can still edit your profile; secrets will not be handed
              out until the check succeeds.
            </p>
          </Notice>
        </div>
      )}

      <Step index="01" title="Confirm your profile" delayMs={60}>
        <p className="mb-5 max-w-xl text-muted">
          Prefilled from GitHub. Correct anything that is wrong.
        </p>
        <Profile member={member} />
      </Step>

      <Step index="02" title="Install riabuild" delayMs={120}>
        <p className="mb-4 max-w-xl text-muted">
          One formula, from the Clubria tap. Homebrew handles updates from here.
        </p>
        <CommandLine command="brew install clubria/tap/riabuild" />
      </Step>

      <Step index="03" title="Run it" delayMs={180}>
        <CommandLine command="riabuild" />
        <p className="mt-4 max-w-xl text-muted">
          The first run signs this machine in through your browser and comes
          straight back to the terminal. Every run after that checks the machine
          and repairs what drifted. Type <span className="mono">exit</span> to
          leave the environment.
        </p>
        {config !== undefined && (
          <p className="mono mt-4 text-muted">
            repo {config.repoSlug} · into {config.defaultProjectPath} · needs
            riabuild v{config.minCliVersion} or newer
          </p>
        )}
      </Step>

      <Step index="04" title="Your machines" delayMs={240}>
        <Sessions />
      </Step>

      {isLead && (
        <>
          <Step index="LEAD" title="Members and roles" delayMs={300}>
            <Members viewerId={member._id} />
          </Step>
          <Step index="LEAD" title="Org configuration" delayMs={320}>
            <OrgSettings />
          </Step>
          <Step index="LEAD" title="Audit log" delayMs={340}>
            <AuditLog />
          </Step>
        </>
      )}
    </>
  );
}
