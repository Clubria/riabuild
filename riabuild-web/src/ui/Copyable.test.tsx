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

  test("bounds the displayed length of a long dash-less value instead of rendering it whole", () => {
    vi.stubGlobal("navigator", {});
    const LONG = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    render(<Copyable value={LONG} label="token" />);
    // Truncated to the same 8-character cap a UUID's first segment gets —
    // never the whole 64-character string, which would overflow its row.
    expect(screen.getByText("abcdefgh…")).toBeVisible();
    expect(screen.queryByText(LONG, { selector: "[aria-hidden]" })).not.toBeInTheDocument();
    expect(screen.getByText(LONG)).toBeInTheDocument(); // still the sr-only / copy source
  });

  test("does not append an ellipsis when the value already fits", () => {
    vi.stubGlobal("navigator", {});
    render(<Copyable value="short" label="value" />);
    // Both the visible (aria-hidden) prefix and the sr-only full value read
    // "short" when nothing was cut — disambiguate by picking the visible one.
    expect(screen.getByText("short", { selector: "[aria-hidden]" })).toBeVisible();
    expect(screen.queryByText("short…")).not.toBeInTheDocument();
  });

  test("announces the copy result to a screen reader without changing the button's own label", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });

    render(<Copyable value={UUID} label="member id" />);
    // aria-label overrides the accessible name outright, so the button's own
    // name must not carry the state — a separate live region does.
    expect(screen.getByRole("button", { name: "Copy member id" })).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Copy member id" }));
    expect(await screen.findByText(/member id copied/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Copy member id" })).toBeInTheDocument();
  });

  test("announces a failed copy to a screen reader", async () => {
    vi.stubGlobal("navigator", {});
    render(<Copyable value={UUID} label="member id" />);
    await userEvent.click(screen.getByRole("button", { name: /copy member id/i }));
    expect(await screen.findByText(/copy failed.*member id/i)).toBeInTheDocument();
  });
});
