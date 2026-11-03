// Screenshots of the deployed UI for the README. Reads only public pages.
// Usage: node scripts/screenshots/shoot.mjs <base-url> <run-id> <out-dir>
import { chromium } from "playwright";

const [base, runId, out] = process.argv.slice(2);
const detail = await (await fetch(`${base}/api/v1/runs/${runId}`)).json();
const verified = detail.trials.filter((t) => t.outcome === "VERIFIED").pop();
if (!verified) throw new Error(`run ${runId} has no VERIFIED trial`);

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1366, height: 900 }, colorScheme: "light", deviceScaleFactor: 1 });
const errors = [];
page.on("pageerror", (e) => errors.push(e.message));

async function shot(path, waitFor, file, height) {
  await page.goto(`${base}${path}`, { waitUntil: "networkidle" });
  await page.waitForSelector(waitFor, { timeout: 60_000 });
  await page.waitForTimeout(1500);
  const full = await page.evaluate(() => document.documentElement.scrollHeight);
  await page.screenshot({ path: `${out}/${file}`, clip: { x: 0, y: 0, width: 1366, height: Math.min(full, height) } });
  console.log(`${file}: ${path}`);
}

await shot("/", ".highlight .tile", "ui-runs.png", 1500);
await shot(`/runs/${runId}`, ".timeline", "ui-run.png", 1500);
await shot(`/trials/${verified.id}`, ".result-strip", "ui-trial.png", 1500);
await browser.close();
if (errors.length) throw new Error(`page errors: ${errors.join("; ")}`);
