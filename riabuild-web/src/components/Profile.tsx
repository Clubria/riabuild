import { useMutation } from "convex/react";
import { FormEvent, useState } from "react";
import { api } from "../../convex/_generated/api";
import { Field, Notice } from "./primitives";

type Member = {
  firstName: string;
  lastName: string;
  email: string;
};

/**
 * Prefilled from the GitHub profile and its verified email list. The developer
 * confirms or corrects it — this is the only thing riabuild asks them to decide.
 */
export function Profile({ member }: { member: Member }) {
  const updateProfile = useMutation(api.members.updateProfile);
  const [firstName, setFirstName] = useState(member.firstName);
  const [lastName, setLastName] = useState(member.lastName);
  const [email, setEmail] = useState(member.email);
  const [state, setState] = useState<"idle" | "saving" | "saved">("idle");
  const [error, setError] = useState<string | null>(null);

  const dirty =
    firstName !== member.firstName ||
    lastName !== member.lastName ||
    email !== member.email;

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    setState("saving");
    setError(null);
    void updateProfile({ firstName, lastName, email })
      .then(() => {
        setState("saved");
        setTimeout(() => setState("idle"), 2000);
      })
      .catch((cause: unknown) => {
        setState("idle");
        setError(
          cause instanceof Error
            ? cause.message.replace(/^.*Uncaught Error:\s*/, "")
            : "Could not save your profile.",
        );
      });
  }

  return (
    <form onSubmit={onSubmit} className="max-w-xl">
      <div className="grid gap-4 sm:grid-cols-2">
        <Field
          label="First name"
          value={firstName}
          onChange={setFirstName}
          autoComplete="given-name"
        />
        <Field
          label="Last name"
          value={lastName}
          onChange={setLastName}
          autoComplete="family-name"
        />
      </div>
      <div className="mt-4">
        <Field
          label="Email"
          value={email}
          onChange={setEmail}
          type="email"
          autoComplete="email"
        />
      </div>
      <div className="mt-5 flex items-center gap-4">
        <button className="btn" disabled={!dirty || state === "saving"}>
          {state === "saving" ? "Saving…" : "Save profile"}
        </button>
        {state === "saved" && <span className="eyebrow text-verified">Saved</span>}
      </div>
      {error !== null && (
        <div className="mt-5">
          <Notice tone="signal" title="Not saved">
            <p>{error}</p>
          </Notice>
        </div>
      )}
    </form>
  );
}
