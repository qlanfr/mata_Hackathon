export const pct = (x: number | null | undefined, digits = 1): string =>
  x == null || Number.isNaN(x) ? "—" : `${(x * 100).toFixed(digits)}%`;

export const ms = (x: number | null | undefined): string => {
  if (x == null || Number.isNaN(x)) return "—";
  if (x >= 100) return `${Math.round(x)} ms`;
  if (x >= 10) return `${x.toFixed(1)} ms`;
  return `${x.toFixed(2)} ms`;
};

export const duration = (millis: number): string => {
  const s = Math.max(0, Math.round(millis / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  return s % 60 === 0 ? `${m}m` : `${m}m ${s % 60}s`;
};

export const offset = (millis: number): string => {
  const s = Math.max(0, Math.floor(millis / 1000));
  return `${String(Math.floor(s / 60)).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}`;
};

export const ago = (ts: number): string => {
  const diff = Date.now() - ts;
  if (diff < 60_000) return "just now";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} min ago`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)} h ago`;
  return new Date(ts).toLocaleDateString(undefined, { month: "short", day: "numeric" });
};

export const clock = (ts: number): string =>
  new Date(ts).toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit", second: "2-digit" });

export const short = (id: string | null | undefined, n = 8): string => (id ? id.slice(-n) : "—");

export const hash = (h: string | null | undefined, n = 10): string => (h ? h.slice(0, n) : "—");
