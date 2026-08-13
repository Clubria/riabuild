/**
 * SSH keys the org issues: a private key a lead pastes once, and the people it
 * is issued to.
 *
 * Everything that keeps this table's plaintext secret bounded is in this file
 * or reachable from it, so read the four rules together rather than one at a
 * time:
 *
 * 1. **Nothing here returns a private key to a browser.** [`issuedKeyView`]
 *    has no such field, so a handler that tried would fail its own `returns`
 *    validator. There is no reveal control and no edit-in-place; changing a key
 *    is [`replaceKey`], which takes a fresh paste.
 * 2. **The derived fields are derived, never accepted.** The browser parses the
 *    same key with the same module so the lead can see what they are storing,
 *    but what is *stored* comes from [`derive`] here. A client that lied would
 *    otherwise decide what fingerprint a lead compares rows by.
 * 3. **Every change is audited, and so is every read.** [`serveForApi`] is the
 *    only function in riabuild that hands out a durable credential, which is
 *    why it is the only one whose reads are logged — see its own comment.
 * 4. **A key bootstraps, it does not replace.** That rule lives in the CLI
 *    (`crates/remote/src/authorise.rs`), but it is why this table can hold one
 *    key for six people without giving up per-developer attribution on the
 *    server.
 *
 * Design: `docs/superpowers/specs/2026-08-13-issued-ssh-keys-design.md`.
 */

import { v } from "convex/values";
import { Doc, Id } from "./_generated/dataModel";
import {
  MutationCtx,
  internalMutation,
  mutation,
  query,
} from "./_generated/server";
import { KeyParseError, parseOpenSshPrivateKey } from "./lib/opensshKey";
import { requireLead, writeAudit } from "./members";

const LABEL = /^[A-Za-z0-9._-]{1,32}$/;

/**
 * What the dashboard reads.
 *
 * Note what is absent, and note that it is absent *by construction* rather than
 * by omission at a call site: there is no `privateKey` in this validator, so a
 * handler that tried to return one would fail its own `returns` check rather
 * than quietly succeeding on the day somebody spreads a document into it.
 */
export const issuedKeyView = v.object({
  _id: v.id("issuedKeys"),
  label: v.string(),
  keyType: v.string(),
  publicKey: v.string(),
  fingerprint: v.string(),
  issuedTo: v.array(v.id("members")),
  updatedAt: v.number(),
});

/** What `GET /api/v1/issued-keys` serves. The one shape that carries a secret. */
const servedKey = v.object({
  id: v.string(),
  label: v.string(),
  keyType: v.string(),
  publicKey: v.string(),
  fingerprint: v.string(),
  privateKey: v.string(),
});

function toView(row: Doc<"issuedKeys">) {
  return {
    _id: row._id,
    label: row.label,
    keyType: row.keyType,
    publicKey: row.publicKey,
    fingerprint: row.fingerprint,
    issuedTo: row.issuedTo,
    updatedAt: row.updatedAt,
  };
}

function validateLabel(input: string): string {
  const label = input.trim();
  if (!LABEL.test(label)) {
    throw new Error(
      "A key's name can hold letters, digits, dots, dashes and underscores, up to 32 of them.",
    );
  }
  return label;
}

/**
 * The parse, with its messages passed through rather than reworded.
 *
 * `KeyParseError`'s sentences are written for a lead reading them in a browser
 * and each one says what to do next — convert the file, remove the passphrase.
 * Rewrapping them in "could not save key" would throw that away and leave the
 * lead with a box that refuses their paste and will not say why.
 */
async function derive(privateKey: string) {
  try {
    return await parseOpenSshPrivateKey(privateKey);
  } catch (error) {
    if (error instanceof KeyParseError) throw new Error(error.message);
    throw error;
  }
}

async function requireFreshLabel(
  ctx: MutationCtx,
  label: string,
  except?: Id<"issuedKeys">,
) {
  const clash = await ctx.db
    .query("issuedKeys")
    .withIndex("by_label", (q) => q.eq("label", label))
    .unique();
  if (clash !== null && clash._id !== except) {
    throw new Error(`There is already an issued key called ${label}.`);
  }
}

export const list = query({
  args: {},
  returns: v.array(issuedKeyView),
  handler: async (ctx) => {
    await requireLead(ctx);
    const rows = await ctx.db.query("issuedKeys").take(200);
    return rows
      .map(toView)
      .sort((a, b) => a.label.localeCompare(b.label));
  },
});

export const create = mutation({
  args: { label: v.string(), privateKey: v.string() },
  returns: v.id("issuedKeys"),
  handler: async (ctx, args) => {
    const actor = await requireLead(ctx);
    const label = validateLabel(args.label);
    await requireFreshLabel(ctx, label);
    const parsed = await derive(args.privateKey);

    const now = Date.now();
    const id = await ctx.db.insert("issuedKeys", {
      label,
      privateKey: args.privateKey,
      publicKey: parsed.publicKey,
      fingerprint: parsed.fingerprint,
      keyType: parsed.keyType,
      issuedTo: [],
      createdBy: actor._id,
      createdAt: now,
      updatedAt: now,
    });
    // The fingerprint, never the key. An audit row is read casually, exported,
    // and kept far longer than the secret it describes.
    await writeAudit(ctx, {
      actorId: actor._id,
      action: "issued_key.created",
      meta: { label, fingerprint: parsed.fingerprint },
    });
    return id;
  },
});

/**
 * Rotation, in one step.
 *
 * The row, its name and its grants all survive; only the secret changes. A lead
 * who had to remove and re-add would take everyone's access away in between,
 * and would have to remember the member list to put it back.
 */
export const replaceKey = mutation({
  args: { id: v.id("issuedKeys"), privateKey: v.string() },
  returns: v.null(),
  handler: async (ctx, args) => {
    const actor = await requireLead(ctx);
    const row = await ctx.db.get("issuedKeys", args.id);
    if (row === null) throw new Error("That issued key is already gone.");
    const parsed = await derive(args.privateKey);

    await ctx.db.patch("issuedKeys", args.id, {
      privateKey: args.privateKey,
      publicKey: parsed.publicKey,
      fingerprint: parsed.fingerprint,
      keyType: parsed.keyType,
      updatedAt: Date.now(),
    });
    await writeAudit(ctx, {
      actorId: actor._id,
      action: "issued_key.replaced",
      meta: {
        label: row.label,
        from: row.fingerprint,
        fingerprint: parsed.fingerprint,
      },
    });
    return null;
  },
});

export const setIssuedTo = mutation({
  args: { id: v.id("issuedKeys"), issuedTo: v.array(v.id("members")) },
  returns: v.null(),
  handler: async (ctx, args) => {
    const actor = await requireLead(ctx);
    const row = await ctx.db.get("issuedKeys", args.id);
    if (row === null) throw new Error("That issued key is already gone.");

    // Every id has to resolve, and the audit entry needs the logins anyway. A
    // dangling id would serve nobody and show as nothing in the panel — the
    // lead would believe someone held a key they could not use.
    const members = new Map<Id<"members">, string>();
    for (const id of args.issuedTo) {
      const member = await ctx.db.get("members", id);
      if (member === null) {
        throw new Error(
          "One of the people you picked is no longer a member. Reload the page and try again.",
        );
      }
      members.set(id, member.githubLogin);
    }

    const before = new Set(row.issuedTo);
    const after = new Set(args.issuedTo);
    const added = [...after].filter((id) => !before.has(id));
    const removed = [...before].filter((id) => !after.has(id));

    const loginOf = async (id: Id<"members">) =>
      members.get(id) ?? (await ctx.db.get("members", id))?.githubLogin ?? "(removed)";

    await ctx.db.patch("issuedKeys", args.id, {
      issuedTo: [...after],
      updatedAt: Date.now(),
    });
    await writeAudit(ctx, {
      actorId: actor._id,
      action: "issued_key.issued",
      meta: {
        label: row.label,
        added: (await Promise.all(added.map(loginOf))).join(","),
        removed: (await Promise.all(removed.map(loginOf))).join(","),
      },
    });
    return null;
  },
});

export const remove = mutation({
  args: { id: v.id("issuedKeys") },
  returns: v.null(),
  handler: async (ctx, args) => {
    const actor = await requireLead(ctx);
    const row = await ctx.db.get("issuedKeys", args.id);
    if (row === null) return null;

    await ctx.db.delete("issuedKeys", args.id);
    await writeAudit(ctx, {
      actorId: actor._id,
      action: "issued_key.removed",
      meta: { label: row.label, fingerprint: row.fingerprint },
    });
    return null;
  },
});

/**
 * What `GET /api/v1/issued-keys` serves, and the audit row that goes with it.
 *
 * An `internalMutation` rather than an `internalQuery`, which is the whole
 * point: a query cannot write, and splitting the read from the log would leave
 * a function that hands out private keys silently beside one that records it —
 * so the next caller would take the keys and not the logging.
 *
 * Sorted by label, and it matters. The CLI probes these in the order it
 * receives them; a fetch that reordered would get in with a different key on
 * every run, and the terminal would name a different one each time.
 */
export const serveForApi = internalMutation({
  args: { memberId: v.id("members") },
  returns: v.array(servedKey),
  handler: async (ctx, args) => {
    const rows = await ctx.db.query("issuedKeys").take(200);
    const mine = rows
      .filter((row) => row.issuedTo.some((id) => id === args.memberId))
      .sort((a, b) => a.label.localeCompare(b.label));

    // The only response in riabuild carrying a durable credential, so it is the
    // only one whose *reads* are logged. A log of grants answers who was
    // entitled to a key; this answers who took a copy of one, which is the
    // question actually asked after somebody leaves.
    await writeAudit(ctx, {
      actorId: args.memberId,
      action: "issued_key.served",
      meta: {
        keys: mine.map((row) => row.label).join(","),
        count: String(mine.length),
      },
    });

    return mine.map((row) => ({
      id: row._id,
      label: row.label,
      keyType: row.keyType,
      publicKey: row.publicKey,
      fingerprint: row.fingerprint,
      privateKey: row.privateKey,
    }));
  },
});
