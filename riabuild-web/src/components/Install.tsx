import { useState } from "react";
import { Button, Command } from "../ui";
import { INSTALL_CHOICES, Platform, guessPlatform } from "../lib/platform";

/**
 * The install instructions, one platform at a time.
 *
 * riabuild ships through three package managers and a developer needs exactly
 * one of them. Showing all three at once means everyone reads two blocks of
 * shell that do not apply to them and then works out which of the remaining
 * ones does — so the page guesses, and the other two stay one click away rather
 * than hidden.
 *
 * `platform` is for the gallery, which shows all three at once because the live
 * panel never does.
 */
export function Install({ platform }: { platform?: Platform }) {
  const [chosen, setChosen] = useState<Platform>(
    () =>
      platform ??
      guessPlatform(typeof navigator === "undefined" ? "" : navigator.userAgent),
  );
  const choice =
    INSTALL_CHOICES.find((option) => option.id === chosen) ??
    INSTALL_CHOICES[0];

  return (
    <div>
      {/* Library `Button`s rather than a hand-rolled radio group. The selected
          one is `primary`, which is how the rest of the console shows an active
          choice, and the aria-label says what pressing it does — three bare
          words would announce as three unrelated actions. */}
      <div className="mb-3 flex flex-wrap gap-1">
        {INSTALL_CHOICES.map((option) => (
          <Button
            key={option.id}
            variant={option.id === chosen ? "primary" : "quiet"}
            onClick={() => setChosen(option.id)}
            aria-label={`Show ${option.label} install instructions`}
          >
            {option.label}
          </Button>
        ))}
      </div>

      <p className="mb-3 max-w-prose text-fg-dim">{choice.audience}</p>
      <Command command={choice.command} />
    </div>
  );
}
