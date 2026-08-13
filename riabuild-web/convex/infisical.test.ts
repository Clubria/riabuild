import { afterEach, describe, expect, test, vi } from "vitest";
import { environmentsForRole, identityForRole } from "./infisical";

afterEach(() => {
  vi.unstubAllEnvs();
});

describe("which environments a role may pull", () => {
  test("a developer and a lead get dev and staging", () => {
    expect(environmentsForRole("developer")).toEqual(["dev", "staging"]);
    expect(environmentsForRole("lead")).toEqual(["dev", "staging"]);
  });

  test("a candidate gets the base environment alone", () => {
    // The same split `identityForRole` makes: a candidate is brokered through
    // the narrower machine identity, so asking for staging on their behalf
    // would only produce an Infisical denial the developer cannot act on.
    expect(environmentsForRole("candidate")).toEqual(["dev"]);
    expect(identityForRole("candidate").name).toBe("mi-candidate");
  });

  test("both environment names come from the deployment", () => {
    vi.stubEnv("INFISICAL_ENVIRONMENT", "development");
    vi.stubEnv("INFISICAL_STAGING_ENVIRONMENT", "stage");
    expect(environmentsForRole("lead")).toEqual(["development", "stage"]);
    expect(environmentsForRole("candidate")).toEqual(["development"]);
  });

  test("a deployment that points both names at one environment pulls it once", () => {
    // Otherwise the CLI would export the same environment twice and write the
    // second copy over the first under a different filename.
    vi.stubEnv("INFISICAL_STAGING_ENVIRONMENT", "dev");
    expect(environmentsForRole("lead")).toEqual(["dev"]);
  });

  test("a deployment can switch staging off entirely", () => {
    // An org with no staging environment in Infisical: asking for one would
    // fail every developer's run rather than degrade.
    vi.stubEnv("INFISICAL_STAGING_ENVIRONMENT", "");
    expect(environmentsForRole("lead")).toEqual(["dev"]);
  });
});
