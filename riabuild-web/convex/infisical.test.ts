import { afterEach, describe, expect, test, vi } from "vitest";
import { environmentsForRole, identityForRole } from "./infisical";

afterEach(() => {
  vi.unstubAllEnvs();
});

describe("which machine identity a role is brokered through", () => {
  function stubLeadIdentity() {
    vi.stubEnv("INFISICAL_LEAD_CLIENT_ID", "lead-id");
    vi.stubEnv("INFISICAL_LEAD_CLIENT_SECRET", "lead-secret");
  }

  test("a lead gets the identity Infisical grants everything", () => {
    stubLeadIdentity();
    const identity = identityForRole("lead");
    expect(identity.name).toBe("mi-lead");
    expect(identity.clientIdVar).toBe("INFISICAL_LEAD_CLIENT_ID");
    expect(identity.clientSecretVar).toBe("INFISICAL_LEAD_CLIENT_SECRET");
  });

  test("a developer and a candidate are unaffected by it", () => {
    // Widening a lead must not widen anybody else: the credential is the only
    // thing that differs, and the permissions behind each are Infisical's.
    stubLeadIdentity();
    expect(identityForRole("developer").name).toBe("mi-developer");
    expect(identityForRole("candidate").name).toBe("mi-candidate");
  });

  test("a deployment without mi-lead keeps its leads on mi-developer", () => {
    // Where every lead was brokered before `mi-lead` existed. Failing their
    // runs until two Convex environment variables are set would cost more than
    // it buys, and the fallback only ever narrows.
    expect(identityForRole("lead").name).toBe("mi-developer");
  });

  test("half a lead identity is not one", () => {
    // A client id typed and a secret still to come is a deployment mid-setup.
    // Authenticating with an incomplete pair fails against Infisical with a
    // 401 nobody can read; the narrower identity that works is the answer.
    vi.stubEnv("INFISICAL_LEAD_CLIENT_ID", "lead-id");
    expect(identityForRole("lead").name).toBe("mi-developer");
  });
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
