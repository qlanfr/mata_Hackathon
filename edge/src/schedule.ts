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

const WEEKDAYS = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/**
 * The same for the weekly vulnerability hunt, `sun 16:00` style (UTC): on that day, from that
 * minute, for one hour. Returns the start of the UTC day when due, otherwise null.
 */
export function weeklyDue(weeklyUtc: string | undefined, now: Date): number | null {
  const m = /^([a-z]+)\s+(\d{1,2}:\d{2})$/.exec((weeklyUtc ?? "").trim().toLowerCase());
  if (!m) return null;
  const day = WEEKDAYS.findIndex((d) => m[1].startsWith(d));
  if (day < 0) return null;
  const today = (now.getUTCDay() + 6) % 7; // getUTCDay: 0 = Sunday
  if (today !== day) return null;
  return scheduleDue(m[2], now);
}
