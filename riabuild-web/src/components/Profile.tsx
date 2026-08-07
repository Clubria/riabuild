import { FormEvent, useState } from "react";
import { useData } from "../data/context";
import { Member } from "../data/types";
import { readError } from "../lib/errors";
import { Alert, Button, Copyable, Field, KeyValue } from "../ui";

/**
 * Prefilled from the GitHub profile and its verified email list. The developer
 * confirms or corrects it — this is the only thing riabuild asks them to decide.
 */
export function Profile({ member }: { member: Member }) {
  const data = useData();
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
    void data
      .updateProfile({ firstName, lastName, email })
      .then(() => {
        setState("saved");
        setTimeout(() => setState("idle"), 2000);
      })
      .catch((cause: unknown) => {
        setState("idle");
        setError(readError(cause, "Could not save your profile."));
      });
  }

  return (
    <form onSubmit={onSubmit} className="max-w-xl">
      <div className="mb-5">
        <KeyValue
          rows={[
            {
              label: "member id",
              value: <Copyable value={member.memberId} label="member id" />,
            },
          ]}
        />
      </div>
      <div className="grid gap-4 sm:grid-cols-2">
        <Field
          label="first name"
          value={firstName}
          onChange={setFirstName}
          autoComplete="given-name"
        />
        <Field
          label="last name"
          value={lastName}
          onChange={setLastName}
          autoComplete="family-name"
        />
      </div>
      <div className="mt-4">
        <Field
          label="email"
          value={email}
          onChange={setEmail}
          type="email"
          autoComplete="email"
        />
      </div>
      <div className="mt-5 flex flex-wrap items-center gap-3">
        <Button
          type="submit"
          variant="primary"
          disabled={!dirty}
          pending={state === "saving"}
          pendingLabel="saving"
        >
          save profile
        </Button>
        {state === "saved" && (
          <span className="text-xs tracking-wider text-ok uppercase">saved</span>
        )}
        {!dirty && state === "idle" && (
          <span className="text-xs text-fg-faint">no changes</span>
        )}
      </div>
      {error !== null && (
        <div className="mt-5">
          <Alert tone="danger" title="Not saved">
            <p className="wrap-value">{error}</p>
          </Alert>
        </div>
      )}
    </form>
  );
}
