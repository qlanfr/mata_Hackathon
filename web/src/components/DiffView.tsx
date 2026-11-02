/** Unified diff with added and removed lines marked. */
export function DiffView({ diff }: { diff: string }) {
  if (!diff.trim()) return <p className="muted small">No changes.</p>;
  const lines = diff.replace(/\n$/, "").split("\n");
  return (
    <pre className="diff" aria-label="Code diff">
      {lines.map((line, i) => {
        const cls = line.startsWith("+++") || line.startsWith("---") ? "meta" : line.startsWith("@@") ? "hunk" : line.startsWith("+") ? "add" : line.startsWith("-") ? "del" : "";
        return (
          <span key={i} className={cls}>
            {line || " "}
          </span>
        );
      })}
    </pre>
  );
}
