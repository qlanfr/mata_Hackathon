import { useEffect, useRef, useState } from "react";
import { getToken, setToken } from "../api";

/** Asks for the admin token used for writes (start and cancel runs). Reading needs no token. */
export function TokenDialog({ open, onClose }: { open: boolean; onClose: (saved: boolean) => void }) {
  const ref = useRef<HTMLDialogElement>(null);
  const [value, setValue] = useState("");

  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    if (open && !dialog.open) {
      setValue(getToken());
      dialog.showModal();
    } else if (!open && dialog.open) {
      dialog.close();
    }
  }, [open]);

  return (
    <dialog ref={ref} onClose={() => onClose(false)} aria-labelledby="token-title">
      <form
        method="dialog"
        onSubmit={(e) => {
          e.preventDefault();
          setToken(value.trim());
          onClose(true);
        }}
      >
        <h2 id="token-title">Admin token</h2>
        <p className="muted small">
          Starting and cancelling runs needs the token set with <code>wrangler secret put ADMIN_TOKEN</code>. It is kept in this
          browser only.
        </p>
        <input id="admin-token" type="password" autoComplete="off" value={value} onChange={(e) => setValue(e.target.value)} placeholder="Paste the token" aria-label="Admin token" />
        <div className="actions">
          {getToken() && (
            <button
              type="button"
              className="btn"
              onClick={() => {
                setToken("");
                setValue("");
                onClose(true);
              }}
            >
              Forget token
            </button>
          )}
          <button type="button" className="btn" onClick={() => onClose(false)}>
            Cancel
          </button>
          <button type="submit" className="btn primary" disabled={!value.trim()}>
            Save token
          </button>
        </div>
      </form>
    </dialog>
  );
}
