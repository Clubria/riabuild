# convex/

The riabuild backend: the dashboard's queries and mutations, the `/api/v1`
contract the CLI depends on (`http.ts`), and the brokers that stand between a
laptop and GitHub, Infisical and ngrok.

Read these before changing anything here:

- **`../CLAUDE.md`** — the invariants. Tokens stored hashed, org membership
  re-verified on every secret-brokering request, the two validators that keep a
  secret out of a browser, and the backfill rule for
  `DEFAULT_CLAUDE_SETTINGS`.
- **`_generated/ai/guidelines.md`** — generated for the pinned Convex version
  and the authority on function syntax, validators and schema rules. Always
  declare `args` and `returns`; anything not called from a browser client is an
  `internalQuery` / `internalMutation` / `internalAction`.
- **`../.claude/skills/riabuild-api/SKILL.md`** — before adding or changing an
  endpoint.

Two local rules the generated guidelines do not cover:

**Every outbound call goes through `lib/http.ts`.** `fetchUpstream` carries a
deadline; a bare `fetch` does not, and an upstream that hangs takes every CLI
request waiting behind it with it.

**No component calls `useQuery`.** `src/data/convexProvider.tsx` is the only
file in `src/` allowed to import from `convex/react` — see `../CLAUDE.md` for
the grep that checks it.
