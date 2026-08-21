/**
 * The team's servers: an address a lead types once, and every developer's CLI
 * reads.
 *
 * Nothing here is a secret, and nothing here may become one. A shared server's
 * key pair, its saved password and its riabuild session stay on the laptop that
 * made them — see `docs/superpowers/specs/2026-08-12-shared-servers-design.md`.
 */

import { v } from "convex/values";
import { internalQuery, mutation, query } from "./_generated/server";
import { requireLead, writeAudit } from "./members";

const NAME = /^[A-Za-z0-9._-]{1,32}$/;
const HOST = /^[A-Za-z0-9.-]{1,253}$/;
const USER = /^[A-Za-z0-9._-]{1,32}$/;

/** What the CLI prefixes a shared server's name with, and so what a name may not start with. */
const DISPLAY_PREFIX = "shared-";

export const sharedServerView = v.object({
  _id: v.id("sharedServers"),
  name: v.string(),
  host: v.string(),
  port: v.number(),
  user: v.string(),
  updatedAt: v.number(),
});

export type Address = {
  name: string;
  host: string;
  port: number;
  user: string;
};

/**
 * The address a lead typed, before it is stored anywhere.
 *
 * Every message here is read by a lead in a browser, so each says what is
 * wrong rather than naming a pattern.
 *
 * The leading-dash rule on `host` is the one that is not cosmetic. riabuild
 * runs `ssh` through its own `CommandRunner` with an argv and no shell, so
 * there is nothing to inject into — but `ssh` reads a leading-dash argument as
 * an option, and `-oProxyCommand=…` sitting in the hostname position runs a
 * command of this row's choosing on every developer's laptop. That is
 * riabuild-web choosing what code runs somewhere else, which is exactly what
 * "the server ships data, never logic" exists to close. The CLI re-checks all
 * of this on the way in — `crates/api/src/remotes.rs` — for the same reason
 * `api::org::version_only` does: the client survives a server that forgets its
 * own check.
 */
export function validateAddress(input: Address): Address {
  const name = input.name.trim();
  const host = input.host.trim();
  const user = input.user.trim();

  if (!NAME.test(name)) {
    throw new Error(
      "A server's name can hold letters, digits, dots, dashes and underscores, up to 32 of them.",
    );
  }
  if (name.toLowerCase().startsWith(DISPLAY_PREFIX)) {
    throw new Error(
      `A name cannot start with "${DISPLAY_PREFIX}" — riabuild adds that itself when it shows the ` +
        "server, so this one would appear as `shared-shared-…`.",
    );
  }
  if (host.startsWith("-")) {
    // Not a formatting rule. `ssh` would read this as an option rather than a
    // hostname, which is a command of this row's choosing on somebody's laptop.
    throw new Error("A hostname cannot start with a dash.");
  }
  if (!HOST.test(host)) {
    throw new Error(
      "A hostname can hold letters, digits, dots and dashes — no spaces, no @, no colon. " +
        "Put the username and the port in their own boxes.",
    );
  }
  if (!Number.isInteger(input.port) || input.port < 1 || input.port > 65535) {
    throw new Error("A port is a whole number between 1 and 65535.");
  }
  if (!USER.test(user)) {
    throw new Error(
      "A username can hold letters, digits, dots, dashes and underscores, up to 32 of them.",
    );
  }
  return { name, host, port: input.port, user };
}

/** `user@host:port`, for the audit log and for the dashboard's own summary line. */
function addressOf(server: Address): string {
  return `${server.user}@${server.host}:${server.port}`;
}

const addressArgs = {
  name: v.string(),
  host: v.string(),
  port: v.number(),
  user: v.string(),
};

/**
 * The dashboard's list. Lead-only, because the section that shows it is: a
 * developer reads the same servers through their CLI's picker, which is where
 * they can act on them.
 */
export const list = query({
  args: {},
  returns: v.array(sharedServerView),
  handler: async (ctx) => {
    await requireLead(ctx);
    const servers = await ctx.db.query("sharedServers").take(200);
    return servers
      .map((server) => ({
        _id: server._id,
        name: server.name,
        host: server.host,
        port: server.port,
        user: server.user,
        updatedAt: server.updatedAt,
      }))
      .sort((a, b) => a.name.localeCompare(b.name));
  },
});

export const add = mutation({
  args: addressArgs,
  returns: v.id("sharedServers"),
  handler: async (ctx, args) => {
    const actor = await requireLead(ctx);
    const address = validateAddress(args);

    // Names are how a developer types a server at the picker, so two rows
    // under one name is two servers nobody can tell apart — and only one of
    // them would ever be reachable.
    const existing = await ctx.db
      .query("sharedServers")
      .withIndex("by_name", (q) => q.eq("name", address.name))
      .unique();
    if (existing !== null) {
      throw new Error(
        `There is already a shared server called ${address.name}.`,
      );
    }

    const now = Date.now();
    const id = await ctx.db.insert("sharedServers", {
      ...address,
      createdBy: actor._id,
      createdAt: now,
      updatedAt: now,
    });
    // Handing every developer a new machine to run `claude` on is an access
    // change, which is what this table is for.
    await writeAudit(ctx, {
      actorId: actor._id,
      action: "shared_server.added",
      meta: { name: address.name, address: addressOf(address) },
    });
    return id;
  },
});

/**
 * Editing an address is editing an identity: riabuild keys a server's SSH key,
 * its saved password and its session off `user@host:port`, so this leaves every
 * developer's credentials pointing at a machine riabuild will no longer be
 * pointed at. The CLI notices and retires the old identity on the next connect
 * — `remote::forget::retire_identity` — which is why the audit entry records
 * both addresses rather than only the new one.
 */
export const update = mutation({
  args: { id: v.id("sharedServers"), ...addressArgs },
  returns: v.null(),
  handler: async (ctx, args) => {
    const actor = await requireLead(ctx);
    const address = validateAddress(args);
    const server = await ctx.db.get("sharedServers", args.id);
    if (server === null) throw new Error("That shared server is already gone.");

    const clash = await ctx.db
      .query("sharedServers")
      .withIndex("by_name", (q) => q.eq("name", address.name))
      .unique();
    if (clash !== null && clash._id !== server._id) {
      throw new Error(
        `There is already a shared server called ${address.name}.`,
      );
    }

    await ctx.db.patch("sharedServers", server._id, {
      ...address,
      updatedAt: Date.now(),
    });
    await writeAudit(ctx, {
      actorId: actor._id,
      action: "shared_server.updated",
      meta: {
        name: address.name,
        from: addressOf(server),
        address: addressOf(address),
      },
    });
    return null;
  },
});

export const remove = mutation({
  args: { id: v.id("sharedServers") },
  returns: v.null(),
  handler: async (ctx, args) => {
    const actor = await requireLead(ctx);
    const server = await ctx.db.get("sharedServers", args.id);
    if (server === null) return null;

    await ctx.db.delete("sharedServers", server._id);
    await writeAudit(ctx, {
      actorId: actor._id,
      action: "shared_server.removed",
      meta: { name: server.name, address: addressOf(server) },
    });
    return null;
  },
});

/**
 * What `GET /api/v1/remotes/shared` serves.
 *
 * `id` is the row id, and the CLI keys its own state by it — so it has to stay
 * stable across a rename *and* across an address edit, which is what a row id
 * is and what a name or an address hash is not.
 */
export const forApi = internalQuery({
  args: {},
  returns: v.array(
    v.object({
      id: v.string(),
      name: v.string(),
      host: v.string(),
      port: v.number(),
      user: v.string(),
    }),
  ),
  handler: async (ctx) => {
    const servers = await ctx.db.query("sharedServers").take(200);
    return servers
      .map((server) => ({
        id: server._id,
        name: server.name,
        host: server.host,
        port: server.port,
        user: server.user,
      }))
      .sort((a, b) => a.name.localeCompare(b.name));
  },
});
