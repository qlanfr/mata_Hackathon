import { useCallback, useRef, useState } from "react";
import { getToken } from "./api";
import { TokenDialog } from "./components/TokenDialog";
import { RunPage } from "./pages/RunPage";
import { RunsPage } from "./pages/RunsPage";
import { TrialPage } from "./pages/TrialPage";
import { Link, usePath } from "./router";

export function App() {
  const path = usePath();
  const [tokenOpen, setTokenOpen] = useState(false);
  const [hasToken, setHasToken] = useState(Boolean(getToken()));
  const resolver = useRef<((saved: boolean) => void) | null>(null);

  /** Opens the token dialog and resolves once it closes; true if a token was saved. */
  const askToken = useCallback(
    () =>
      new Promise<boolean>((resolve) => {
        resolver.current = resolve;
        setTokenOpen(true);
      }),
    [],
  );

  const onTokenClose = (saved: boolean) => {
    setTokenOpen(false);
    setHasToken(Boolean(getToken()));
    resolver.current?.(saved && Boolean(getToken()));
    resolver.current = null;
  };

  const run = path.match(/^\/runs\/([\w-]+)$/);
  const trial = path.match(/^\/trials\/([\w-]+)$/);
  let page;
  if (run) page = <RunPage id={run[1]} askToken={askToken} />;
  else if (trial) page = <TrialPage id={trial[1]} />;
  else if (path === "/" || path === "/runs") page = <RunsPage askToken={askToken} />;
  else
    page = (
      <div className="empty">
        <p>This page does not exist.</p>
        <Link to="/">Back to runs</Link>
      </div>
    );

  return (
    <>
      <div className="topbar">
        <div className="topbar-inner">
          <Link to="/" className="brand">
            <span className="brand-mark" aria-hidden="true">
              <svg width="16" height="16" viewBox="0 0 32 32">
                <path d="M4 24l7-12 6 7 4-6 7 11" stroke="white" strokeWidth="3.5" fill="none" strokeLinecap="round" strokeLinejoin="round" />
              </svg>
            </span>
            Experiment Engineer <small>AIOps</small>
          </Link>
          <span className="spacer" />
          <span className="badge plain">scenario · n-plus-one</span>
          <button className="btn small" onClick={() => void askToken()}>
            {hasToken ? "Admin token set" : "Set admin token"}
          </button>
        </div>
      </div>
      <main className="container">
        {page}
        <footer className="note">
          A Meta model reads production evidence and proposes one change at a time. A Rust harness builds each change in an isolated
          runner, checks it against golden responses and measures it against the unchanged baseline. Only the evaluator decides.
        </footer>
      </main>
      <TokenDialog open={tokenOpen} onClose={onTokenClose} />
    </>
  );
}
