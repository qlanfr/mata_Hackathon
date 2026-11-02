# 배포와 준비 작업

## 1. 현재 상태

| 항목 | 상태 |
| --- | --- |
| D1 `aiops-control` | 생성 완료 (WNAM, id `40be013f-ab7b-40eb-889a-40454179225f`). 스키마는 배포할 때 마이그레이션으로 적용합니다. |
| R2 `aiops-artifacts` | 생성 완료. 64 KB를 넘는 아티팩트만 여기에 저장합니다. |
| 컨테이너 이미지 | GitHub Actions의 **Container E2E** 워크플로가 두 이미지를 빌드하고, Docker 안에서 전체 실행 1회를 돌려 검증합니다. Cloudflare 자격 증명은 필요 없습니다. |
| Worker `aiops-edge` | 배포 완료: https://aiops-edge.yh9365.workers.dev . 배포는 GitHub Actions에서 실행합니다(아래 3절). Claude가 작업하는 클라우드 환경은 네트워크 정책 때문에 `api.cloudflare.com`, `registry.cloudflare.com`, `*.workers.dev`에 접속할 수 없습니다. |
| Meta API | 연결됨: `META_API_KEY`(GitHub secret)와 `edge/wrangler.jsonc`의 `META_MODEL = "muse-spark-1.3"`. Meta contract 워크플로가 실제 API로 형식을 확인하고, 키로 쓸 수 있는 모델 목록을 보여 줍니다(`.github/meta-contract-request`에 모델 ID를 적고 푸시). |

계정에 이미 있는 Worker와 D1(`bid-analytics-v3` 등)은 건드리지 않습니다.

## 2. 사용자가 해야 하는 일

| 할 일 | 메모 |
| --- | --- |
| Cloudflare API 토큰 만들기 | 3절 1단계. 토큰 값은 GitHub secret에만 넣고, 채팅에는 붙여 넣지 마세요. |
| GitHub 저장소 secret 3개 등록 | 3절 2단계: `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID`, `ADMIN_TOKEN` |
| 등록했다고 Claude에게 알리기 | Claude가 **Deploy edge** 워크플로를 실행하고 로그를 보면서 실패를 고칩니다. |
| Meta API 키와 모델 ID | 키는 `META_API_KEY` secret으로, 모델 ID는 `edge/wrangler.jsonc`의 `META_MODEL`에 넣습니다. 두 값이 모두 있어야 Meta provider를 씁니다. |
| 해커톤 등록·규정 확인·데모 영상·최종 제출 | 기존 코드 사용 규정과 필수 제출 항목을 확인해 주세요. |

## 3. 배포 방법 A (권장): GitHub Actions

1. **API 토큰.** Cloudflare 대시보드에서 Manage account → Account API tokens → Create Token으로 갑니다. **Edit Cloudflare Workers** 템플릿을 고르고, 권한에 Account · **Containers** · Edit와 Account · **D1** · Edit를 추가한 뒤 이 계정으로 범위를 한정합니다.
   - Cloudflare 문서상 바인딩(R2·D1)이 있는 Worker를 배포하는 데에는 바인딩 대상 리소스의 권한이 따로 필요하지 않습니다. 다만 D1 권한은 `wrangler d1 migrations apply --remote` 때문에 필요합니다.
   - 권한 이름은 화면에 따라 Edit 또는 Write로 표시됩니다. 권한이 부족하면 워크플로의 `Check the API token` 단계나 배포 단계에서 실패하므로, 오류에 나온 권한을 추가하면 됩니다.
2. **Secret 등록.** GitHub의 `qlanfr/mata_Hackathon` → Settings → Secrets and variables → Actions → New repository secret에서 다음을 등록합니다.

   | 이름 | 값 |
   | --- | --- |
   | `CLOUDFLARE_API_TOKEN` | 1단계에서 만든 토큰 |
   | `CLOUDFLARE_ACCOUNT_ID` | (선택) Account ID. 토큰이 한 계정에만 속하면 워크플로가 토큰에서 계정을 알아내므로 없어도 됩니다. 값이 틀리면 경고를 남기고 토큰의 계정을 씁니다. |
   | `ADMIN_TOKEN` | 16자 이상의 임의 문자열(예: `openssl rand -hex 24`). 화면에서 run을 시작할 때 이 값을 입력합니다. |
   | `META_API_KEY` | (선택) Meta API 키 |

3. **실행.** Actions 탭 → **Deploy edge** → Run workflow를 누릅니다. `.github/deploy-request` 파일에 한 줄을 추가해 브랜치에 푸시해도 실행됩니다(Claude는 이 방법을 씁니다: Claude의 GitHub 연동에는 워크플로 수동 실행 권한이 없습니다). 워크플로는 다음 순서로 진행합니다.
   1. secret 확인
   2. 토큰 확인(`wrangler whoami`)
   3. 타입 검사와 화면 빌드
   4. D1 마이그레이션
   5. 배포(이미지 두 개 빌드·푸시)
   6. Worker secret 설정
   7. 배포된 URL에서 전체 run 1회 실행

   Worker URL은 실행 요약(Summary)에 나옵니다. run 결과와 이벤트는 `deploy-edge` 아티팩트에 들어 있습니다.

첫 배포 직후에는 컨테이너를 준비하느라 몇 분 동안 `/api/*`가 오류를 낼 수 있습니다(Cloudflare 문서에 안내된 동작). 그래서 7단계는 API가 응답할 때까지 최대 15분 기다린 뒤 run을 시작합니다.

## 4. 배포 방법 B: 내 PC에서

Docker가 실행 중이어야 합니다. 첫 배포는 두 이미지(Rust 빌드 포함)를 만드느라 오래 걸립니다. Apple Silicon Mac에서는 amd64 에뮬레이션으로 빌드하므로 30분 넘게 걸릴 수 있습니다.

```bash
git clone https://github.com/qlanfr/mata_Hackathon.git && cd mata_Hackathon && git checkout claude/busy-cannon-5b5n40
```

```bash
cd edge && npm ci && npx wrangler login
```

```bash
npx wrangler d1 migrations apply aiops-control --remote
```

`npm run deploy`는 React 화면(`web`)을 빌드한 뒤 `wrangler deploy`를 실행합니다.

```bash
npm run deploy
```

```bash
npx wrangler secret put ADMIN_TOKEN
```

Meta 키를 받으면:

```bash
npx wrangler secret put META_API_KEY
```

배포 전에 이미지만 내 PC에서 확인하려면(배포 없이 Docker로 전체 run 1회):

```bash
scripts/container-e2e.sh
```

## 5. 배포 후 확인

전체 루프(baseline → 관측 AI → 실험 AI → 검증 → 판정)를 1회 실행하고 결과를 출력합니다. Deploy edge 워크플로의 마지막 단계도 이 스크립트를 씁니다.

```bash
BASE_URL=https://aiops-edge.<서브도메인>.workers.dev ADMIN_TOKEN=<토큰> scripts/drive-run.sh
```

runner만 따로 확인하려면(A/A 노이즈, 후보 빌드 시간, 누락 여부):

```bash
EDGE_URL=https://aiops-edge.<서브도메인>.workers.dev ADMIN_TOKEN=<토큰> scripts/cf-runner-smoke.sh
```

진행 상황(SSE):

```bash
curl -N "$BASE_URL/api/v1/runs/<run_id>/events" -H 'accept: text/event-stream'
```

출력과 `.work/` 아래 JSON을 Claude에게 전달하면 측정 계약을 조정합니다.

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
