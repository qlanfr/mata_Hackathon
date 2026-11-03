import { useCallback, useEffect, useRef, useState } from "react";
import { api, type RunEvent } from "./api";

/** Loads data, reloads on demand, and optionally polls while `pollMs` is set. */
export function useLoad<T>(load: () => Promise<T>, deps: unknown[], pollMs?: number) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const loadRef = useRef(load);
  loadRef.current = load;

  const reload = useCallback(async () => {
    try {
      setData(await loadRef.current());
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    setData(null);
    void reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);

  useEffect(() => {
    if (!pollMs) return;
    const timer = window.setInterval(() => void reload(), pollMs);
    return () => window.clearInterval(timer);
  }, [pollMs, reload]);

  return { data, error, reload };
}

const EVENT_KINDS = [
  "run_created", "alert_merged", "cancel_requested", "state", "job_submitted", "job_waiting", "job_lost",
  "baseline_measured", "agent_started", "model_call", "tool_call", "agent_repair", "agent_wrap_up", "observation", "plan",
  "duplicate_patch", "patch_refused", "cancel_sent", "step_error",
];

/**
 * Live run events over SSE, resuming from the last sequence number. If the stream cannot be
 * kept open, falls back to polling the same cursor every two seconds.
 */
export function useRunEvents(runId: string) {
  const [events, setEvents] = useState<RunEvent[]>([]);
  const [live, setLive] = useState(false);
  const lastSeq = useRef(0);

  useEffect(() => {
    setEvents([]);
    lastSeq.current = 0;
    let closed = false;
    let poll: number | undefined;
    let failures = 0;

    const add = (incoming: RunEvent[]) => {
      const fresh = incoming.filter((e) => e.seq > lastSeq.current);
      if (!fresh.length) return;
      lastSeq.current = fresh[fresh.length - 1].seq;
      setEvents((prev) => [...prev, ...fresh]);
    };

    const startPolling = () => {
      if (poll !== undefined || closed) return;
      const tick = async () => {
        try {
          const { events: page } = await api.events(runId, lastSeq.current);
          add(page);
        } catch {
          // Keep polling; the next tick retries.
        }
      };
      void tick();
      poll = window.setInterval(tick, 2000);
    };

    const source = new EventSource(`/api/v1/runs/${runId}/events`);
    const onEvent = (msg: MessageEvent) => {
      failures = 0;
      setLive(true);
      try {
        add([JSON.parse(msg.data) as RunEvent]);
      } catch {
        // Ignore malformed frames.
      }
    };
    EVENT_KINDS.forEach((k) => source.addEventListener(k, onEvent as EventListener));
    source.addEventListener("end", () => {
      source.close();
      setLive(false);
    });
    source.onerror = () => {
      failures += 1;
      setLive(false);
      if (failures >= 3) {
        source.close();
        startPolling();
      }
    };

    return () => {
      closed = true;
      source.close();
      if (poll !== undefined) window.clearInterval(poll);
    };
  }, [runId]);

  return { events, live };
}
