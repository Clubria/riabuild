// @vitest-environment jsdom
import { render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import { DataContext } from "../data/context";
import { SCENARIOS } from "../dev/scenarios";
import { Profile } from "./Profile";

describe("Profile", () => {
  test("a developer sees their own member id in their profile", () => {
    const data = SCENARIOS.developer();
    const member = data.viewer.state === "ready" ? data.viewer.value : null;
    if (member === null) throw new Error("the developer fixture always has a viewer");

    render(
      <DataContext.Provider value={data}>
        <Profile member={member} />
      </DataContext.Provider>,
    );

    expect(screen.getByText("member id")).toBeVisible();
    // The sr-only / copy-source span inside Copyable carries the full value —
    // this fails if Profile stops passing `member.memberId` through.
    expect(screen.getByText(member.memberId)).toBeInTheDocument();
  });
});
