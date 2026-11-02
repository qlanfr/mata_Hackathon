# 배포와 준비 작업

## 1. 현재 상태

| 항목 | 상태 |
| --- | --- |
| D1 `aiops-control` | 생성 완료 (WNAM, id `40be013f-ab7b-40eb-889a-40454179225f`). 스키마는 배포 때 마이그레이션으로 적용합니다. |
| R2 `aiops-artifacts` | 생성 완료. 64 KB를 넘는 아티팩트만 저장합니다. |
| Worker `aiops-edge` | 화면·API·컨테이너 코드는 준비됐고, 배포는 사용자가 실행합니다(이 개발 환경에는 Docker 데몬이 없습니다). 배포 후 Worker URL을 브라우저로 열면 화면이 나옵니다. |
| Meta API | 키가 없으면 Replay provider로 동작합니다. 실행 기록과 화면에 `replay`로 표시됩니다. |

계정에 이미 있는 Worker와 D1(`bid-analytics-v3` 등)은 건드리지 않습니다.

## 2. 사용자가 해야 하는 일

| 할 일 | 메모 |
| --- | --- |
| `ADMIN_TOKEN` 만들기 | 16자 이상. 예: `openssl rand -hex 24`. 쓰기 API(`POST /api/*`)와 `/runner/*`를 보호합니다. |
| 배포 (아래 A 또는 B) | Apple Silicon Mac은 amd64 에뮬레이션 빌드라 30분 이상 걸릴 수 있습니다. 이 경우 B가 빠릅니다. |
| Meta API 키와 모델 ID | 키는 `META_API_KEY` secret, 모델 ID는 `edge/wrangler.jsonc`의 `META_MODEL`에 넣습니다. 둘 다 있어야 Meta provider를 씁니다. |
| 해커톤 등록·규정 확인·데모 영상·최종 제출 | 기존 코드 사용 규정과 필수 제출 항목을 확인해 주세요. |

## 3. 배포 방법 A: 내 PC에서

Docker가 실행 중이어야 합니다. 첫 배포는 두 이미지(Rust 빌드 포함)를 만드느라 오래 걸립니다.

```bash
git clone https://github.com/qlanfr/mata_Hackathon.git && cd mata_Hackathon && git checkout claude/busy-cannon-5b5n40
```

```bash
cd edge && npm ci && npx wrangler login
```

```bash
npx wrangler d1 migrations apply aiops-control --remote
```

```bash
npx wrangler secret put ADMIN_TOKEN
```

`npm run deploy`는 React 화면(`web`)을 빌드한 뒤 `wrangler deploy`를 실행합니다.

```bash
npm run deploy
```

Meta 키를 받으면:

```bash
npx wrangler secret put META_API_KEY
```

## 4. 배포 방법 B: GitHub Actions

1. Cloudflare 대시보드의 Account API tokens에서 **Edit Cloudflare Workers** 템플릿으로 토큰을 만들고, D1 Edit과 Workers R2 Storage Edit 권한을 추가합니다. Containers 항목이 따로 보이면 Edit으로 추가합니다. 컨테이너 배포에 필요한 정확한 권한 이름은 확인하지 못했으므로, 권한 오류가 나면 오류 메시지에 나온 권한을 추가합니다.
2. GitHub 저장소 Settings → Secrets and variables → Actions에 `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID`, `ADMIN_TOKEN`(필수)과 `META_API_KEY`(선택)를 등록합니다.
3. Actions 탭에서 **Deploy edge**를 수동 실행합니다. 마이그레이션 → 배포 → secret 설정 순서로 진행합니다.

## 5. 배포 후 확인

runner만 먼저 확인 (A/A 노이즈, 후보 빌드 시간, 누락 여부):

```bash
EDGE_URL=https://aiops-edge.<서브도메인>.workers.dev ADMIN_TOKEN=<토큰> scripts/cf-runner-smoke.sh
```

전체 루프 실행 (baseline → 관측 AI → 실험 AI → 검증 → 판정):

```bash
curl -s -X POST "$EDGE_URL/api/v1/runs" -H "authorization: Bearer $ADMIN_TOKEN" -H 'content-type: application/json' -d '{}'
```

진행 상황(SSE):

```bash
curl -N "$EDGE_URL/api/v1/runs/<run_id>/events" -H 'accept: text/event-stream'
```

결과:

```bash
curl -s "$EDGE_URL/api/v1/runs/<run_id>"
```

출력과 `.work/cf/*.json`을 Claude에게 전달하면 측정 계약을 조정합니다.

## 6. 로컬 개발 (Docker 없이)

PostgreSQL 서버 바이너리, Rust, k6가 필요합니다. root로 실행할 때는 DB 서버를 일반 사용자로 띄우도록 `--pg-user`를 줍니다.

```bash
cargo build --release -p aiops-runner -p aiopsd
```

```bash
./target/release/aiops-runner --pg-user postgres serve --listen 127.0.0.1:18080
```

```bash
./target/release/aiopsd --listen 127.0.0.1:18090 --runner-url http://127.0.0.1:18080
```

`aiopsd`는 기본값으로 `.work/control.db`(SQLite, 같은 마이그레이션 파일 사용)와 `.work/artifacts`를 씁니다.

## 7. 격리 수준에 대한 메모

- runner 컨테이너는 인터넷이 차단되고 바인딩·비밀값·outbound handler가 없어 D1·R2에 닿지 못합니다. 후보 서비스는 환경 변수를 비운 상태로 실행됩니다.
- aiopsd 컨테이너는 Meta API 때문에 인터넷이 열려 있지만, D1·R2·runner에는 Worker relay(`d1.internal`, `r2.internal`, `runner.internal`)로만 접근합니다. R2 relay는 `runs/` 아래 키만 허용합니다.
- 시나리오·golden 케이스·평가 계약은 root 소유(`/opt/aiops`)라 runner 사용자가 수정할 수 없습니다.
- 후보 서비스와 runner는 같은 사용자로 실행됩니다. 통제된 데모 코드에는 충분하지만, 임의의 고객 코드를 돌리는 단계에서는 후보마다 별도 사용자나 컨테이너로 나눠야 합니다.
