// @vitest-environment jsdom
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";
import { Copyable } from "./Copyable";

const UUID = "550e8400-e29b-41d4-a716-446655440000";

describe("Copyable", () => {
  test("shows a short form but copies and announces the whole value", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });

    render(<Copyable value={UUID} label="member id" />);
    // Truncated on screen, complete to a screen reader and to the clipboard.
    expect(screen.getByText("550e8400…")).toBeVisible();
    expect(screen.getByText(UUID)).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: /copy member id/i }));
    expect(writeText).toHaveBeenCalledWith(UUID);
    expect(await screen.findByRole("button", { name: /copy member id/i })).toHaveTextContent("copied");
  });

  test("a clipboard that is absent is a rendered state, not an unhandled rejection", async () => {
    // e2e/helpers.ts asserts no unhandled rejections on every page, and
    // navigator.clipboard is undefined in an insecure context.
    vi.stubGlobal("navigator", {});
    render(<Copyable value={UUID} label="member id" />);
    await userEvent.click(screen.getByRole("button", { name: /copy member id/i }));
    expect(screen.getByRole("button", { name: /copy member id/i })).toHaveTextContent("copy failed");
  });

  test("an empty value does not render an empty button", () => {
    vi.stubGlobal("navigator", {});
    render(<Copyable value="" label="member id" />);
    expect(screen.getByRole("button", { name: /copy member id/i })).toBeVisible();
  });
});
