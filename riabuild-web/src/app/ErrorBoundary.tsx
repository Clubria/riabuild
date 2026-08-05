import { Component, ErrorInfo, ReactNode } from "react";
import { Button, Panel } from "../ui";

type Props = {
  children: ReactNode;
  /** Names the failing region when the boundary wraps one panel rather than the app. */
  label?: string;
};

type State = { error: Error | null; stack: string | null };

/**
 * The last line of defence. A thrown render is a terminal that dumped core, not
 * a white page.
 *
 * The message and component stack are shown only in dev builds. In production
 * an error string can carry backend detail a signed-out visitor should not see,
 * so it is replaced with a fixed line.
 */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null, stack: null };

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    this.setState({ stack: info.componentStack ?? null });
    console.error("riabuild: render failed", error, info.componentStack);
  }

  render() {
    const { error, stack } = this.state;
    if (error === null) return this.props.children;

    const scope = this.props.label;
    return (
      <Panel
        title={scope === undefined ? "core dumped" : `core dumped — ${scope}`}
        index="err"
        tone="danger"
      >
        <p className="max-w-prose text-fg-dim">
          {scope === undefined
            ? "riabuild could not draw this page."
            : `riabuild could not draw ${scope}. The rest of the page is unaffected.`}
        </p>

        {import.meta.env.DEV ? (
          <pre className="mt-4 max-h-64 overflow-auto border border-rule bg-bg-sunk p-3 text-xs whitespace-pre-wrap text-danger wrap-value">
            {error.message}
            {stack !== null && (
              <span className="text-fg-faint">{"\n" + stack}</span>
            )}
          </pre>
        ) : (
          <p className="mt-4 text-xs text-fg-faint">
            The details were written to the browser console.
          </p>
        )}

        <div className="mt-5 flex flex-wrap gap-2">
          <Button variant="primary" onClick={() => window.location.reload()}>
            reload
          </Button>
          {scope === undefined && (
            <Button variant="quiet" href="/">
              cd /
            </Button>
          )}
        </div>
      </Panel>
    );
  }
}
