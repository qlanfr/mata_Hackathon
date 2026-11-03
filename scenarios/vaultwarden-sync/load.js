// Fixed-rate load on GET /api/sync for the seeded user. The runner logs in once per job and
// passes the bearer token in AUTH_TOKEN, so every request is the same authenticated sync.
import http from 'k6/http';
import { Counter } from 'k6/metrics';

const okResponses = new Counter('ok_responses');
const badStatus = new Counter('bad_status');
const timeouts = new Counter('timeouts');
const netErrors = new Counter('net_errors');

const RATE = parseInt(__ENV.RATE, 10);
const DURATION_S = parseInt(__ENV.DURATION_S, 10);
const TIMEOUT = `${__ENV.TIMEOUT_MS}ms`;
const PARAMS = { timeout: TIMEOUT, headers: { Authorization: `Bearer ${__ENV.AUTH_TOKEN}` } };

export const options = {
  discardResponseBodies: true,
  summaryTrendStats: ['avg', 'min', 'med', 'max', 'p(50)', 'p(95)', 'p(99)'],
  scenarios: {
    load: {
      executor: 'constant-arrival-rate',
      rate: RATE,
      timeUnit: '1s',
      duration: `${DURATION_S}s`,
      preAllocatedVUs: parseInt(__ENV.PRE_VUS, 10),
      maxVUs: parseInt(__ENV.MAX_VUS, 10),
      gracefulStop: '5s',
    },
  },
  // Always passes. Declaring it makes k6 export the successful-response latency submetric.
  thresholds: {
    'http_req_duration{expected_response:true}': ['p(95)>=0'],
  },
};

export default function () {
  const res = http.get(`${__ENV.TARGET}/api/sync?excludeDomains=true`, PARAMS);
  if (res.status === 200) {
    okResponses.add(1);
  } else if (res.error_code === 1050) {
    timeouts.add(1);
  } else if (res.status === 0) {
    netErrors.add(1);
  } else {
    badStatus.add(1);
  }
}

export function handleSummary(data) {
  return { [__ENV.SUMMARY_PATH]: JSON.stringify(data) };
}
