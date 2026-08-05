import { Button, Panel } from "../ui";

/**
 * A 404 in the idiom of the thing it is imitating. The path is rendered as text
 * — it comes from the URL bar, so it is attacker-controlled and must never be
 * interpreted as markup.
 */
export function NotFound({ path }: { path: string }) {
  return (
    <div className="mx-auto max-w-xl py-6">
      <Panel title="404" tone="danger" index="err">
        <p className="wrap-value">
          <span aria-hidden="true" className="mr-2 text-fg-faint">
            $
          </span>
          <span className="text-fg-dim">riabuild </span>
          <span className="text-danger">{path}</span>
        </p>
        <p className="mt-2 text-fg-dim wrap-value">
          command not found: {path}
        </p>
        <p className="mt-5 max-w-prose text-fg-dim">
          riabuild has two pages: the dashboard, and the approval screen the CLI
          opens for you. Nothing lives here.
        </p>
        <div className="mt-5">
          <Button variant="primary" href="/">
            cd /
          </Button>
        </div>
      </Panel>
    </div>
  );
}
