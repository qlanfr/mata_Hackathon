import { useEffect, useRef, useState } from "react";
import type { Pair } from "../api";
import { ms, pct } from "../format";

/**
 * Paired p95 bars, one group per measurement pair, drawn to one scale. Horizontal bars keep the
 * labels readable on phones.
 */
export function PairsChart({ pairs }: { pairs: Pair[] }) {
  // Draw at the container's real width so labels keep their size on phones.
  const wrap = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(640);
  useEffect(() => {
    const el = wrap.current;
    if (!el) return;
    const observer = new ResizeObserver(([entry]) => setWidth(Math.max(280, Math.round(entry.contentRect.width))));
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  if (!pairs.length) return <p className="muted small">No load measurements for this trial.</p>;
  const max = Math.max(...pairs.flatMap((p) => [p.baseline.p95_ms, p.candidate.p95_ms]), 1);
  const narrow = width < 480;
  const left = narrow ? 86 : 92;
  const right = narrow ? 96 : 120;
  const plot = width - left - right;
  const rowH = 22;
  const groupH = rowH * 2 + 18;
  const top = 8;
  const height = top + pairs.length * groupH + 18;
  const x = (v: number) => left + (v / max) * plot;
  const ticks = niceTicks(max);

  return (
    <div className="chart" ref={wrap}>
      <svg width={width} height={height} viewBox={`0 0 ${width} ${height}`} role="img" aria-label={`p95 latency per measurement pair. ${pairs.map((p) => `Pair ${p.index + 1}: baseline ${ms(p.baseline.p95_ms)}, candidate ${ms(p.candidate.p95_ms)}`).join("; ")}`}>
        {ticks.map((t) => (
          <g key={t}>
            <line className="grid" x1={x(t)} x2={x(t)} y1={top - 4} y2={height - 16} />
            <text className="label" x={x(t)} y={height - 3} textAnchor="middle">{t} ms</text>
          </g>
        ))}
        {pairs.map((p, i) => {
          const y0 = top + i * groupH;
          const change = p.baseline.p95_ms > 0 ? (p.baseline.p95_ms - p.candidate.p95_ms) / p.baseline.p95_ms : 0;
          return (
            <g key={p.index}>
              <text className="label" x={0} y={y0 + rowH - 6}>Pair {p.index + 1}</text>
              <text className="label" x={0} y={y0 + rowH * 2 - 6}>{p.order === "CB" ? "cand. first" : "base first"}</text>
              <rect className="bar-b" x={left} y={y0 + 2} width={Math.max(1, x(p.baseline.p95_ms) - left)} height={rowH - 6} rx={3} />
              <text className="value" x={x(p.baseline.p95_ms) + 6} y={y0 + rowH - 7}>{ms(p.baseline.p95_ms)}</text>
              <rect className="bar-c" x={left} y={y0 + rowH + 2} width={Math.max(1, x(p.candidate.p95_ms) - left)} height={rowH - 6} rx={3} />
              <text className="value" x={x(p.candidate.p95_ms) + 6} y={y0 + rowH * 2 - 7}>{ms(p.candidate.p95_ms)}</text>
              <text className={`delta${change < 0 ? " neg" : ""}`} x={width - 4} y={y0 + rowH + 4} textAnchor="end">
                {change >= 0 ? "−" : "+"}{pct(Math.abs(change))}
              </text>
            </g>
          );
        })}
      </svg>
      <div className="legend">
        <span><i style={{ background: "var(--line-strong)" }} />Baseline p95</span>
        <span><i style={{ background: "var(--ctl)" }} />Candidate p95</span>
        <span>Successful responses only, same arrival rate, pairs alternate order</span>
      </div>
    </div>
  );
}

function niceTicks(max: number): number[] {
  const raw = max / 4;
  const mag = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 5, 10].map((m) => m * mag).find((s) => s >= raw) ?? raw;
  const out: number[] = [];
  for (let t = 0; t <= max + 1e-9; t += step) out.push(Number(t.toFixed(6)));
  return out;
}
