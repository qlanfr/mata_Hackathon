/**
 * When the cron must wake aiopsd for the daily scheduled run. aiopsd starts that run itself on its
 * next tick, but it sleeps after 15 idle minutes, and the cron otherwise wakes it only while a run
 * is unfinished. So from the scheduled minute on, for one hour, the cron wakes it until the day's
 * scheduled run exists. The hour bounds the cost when the run cannot start (for example, the daily
 * run limit is reached).
 *
 * Returns the start of the UTC day (epoch ms) when the schedule is due now, otherwise null.
 */
export function scheduleDue(dailyRunUtc: string | undefined, now: Date): number | null {
  const m = /^(\d{1,2}):(\d{2})$/.exec((dailyRunUtc ?? "").trim());
  if (!m) return null;
  const at = Number(m[1]) * 60 + Number(m[2]);
  if (Number(m[1]) > 23 || Number(m[2]) > 59) return null;
  const minute = now.getUTCHours() * 60 + now.getUTCMinutes();
  if (minute < at || minute >= at + 60) return null;
  return Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate());
}
