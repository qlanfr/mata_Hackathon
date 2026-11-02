# 배포와 준비 작업

## 1. 사용자가 해야 하는 일

| 할 일 | 필요한 시점 | 메모 |
| --- | --- | --- |
| Meta API 키와 사용할 모델 ID 받기 | 2일차(11/3) 오전 | 키가 없어도 Replay provider로 계속 개발할 수 있습니다. 데모 영상에는 Replay 실행임을 표시합니다. |
| 배포 방법 정하기 (아래 A 또는 B) | 1일차 | 이 개발 환경에는 Docker 데몬이 없어서 컨테이너 이미지를 빌드할 수 없습니다. |
| `ADMIN_TOKEN` 만들기 | 첫 배포 전 | 16자 이상이어야 합니다. 예: `openssl rand -hex 24`. 쓰기 API와 runner 경로를 보호합니다. |
| R2 사용 설정 | 2일차 | 처음 쓰는 계정이면 대시보드에서 R2 약관에 동의해야 합니다. |
| D1 DB `aiops-control`, R2 버킷 `aiops-artifacts` 만들기 | 2일차 | Claude의 Cloudflare 연결로 대신 만들 수 있습니다(허락 필요). 직접 만들 때는 아래 명령을 씁니다. |
| 해커톤 등록·규정 확인·데모 영상·최종 제출 | 3일차~11/5 오전 | 기존 코드 사용 규정과 필수 제출 항목을 확인해 주세요. |

이번 배포는 새 이름(`aiops-edge`, `aiops-control`, `aiops-artifacts`)만 사용합니다. 계정에 이미 있는 Worker와 D1(`bid-analytics-v3` 등)은 건드리지 않습니다.

```bash
npx wrangler d1 create aiops-control --location wnam
```

```bash
npx wrangler r2 bucket create aiops-artifacts
```

## 2. 배포 방법 A: 내 PC에서

Docker Desktop(또는 호환 엔진)이 실행 중이어야 합니다. 첫 배포에서는 Rust 빌드가 포함된 이미지를 만드느라 10분 이상 걸릴 수 있습니다.

```bash
git clone https://github.com/qlanfr/mata_Hackathon.git
```

```bash
cd mata_Hackathon && git checkout claude/busy-cannon-5b5n40 && cd edge && npm ci
```

```bash
npx wrangler login
```

```bash
npx wrangler secret put ADMIN_TOKEN
```

```bash
npx wrangler deploy
```

## 3. 배포 방법 B: GitHub Actions

1. Cloudflare 대시보드의 Account API tokens에서 토큰을 만듭니다. **Edit Cloudflare Workers** 템플릿으로 시작합니다. 2일차에 쓰려면 D1 Edit과 Workers R2 Storage Edit 권한도 추가합니다. 권한 목록에 Containers 항목이 따로 보이면 Edit으로 추가합니다. 컨테이너 배포에 필요한 정확한 권한 이름은 아직 확인하지 못했으므로, 첫 실행이 권한 오류로 실패하면 오류 메시지에 나온 권한을 추가합니다.
2. GitHub 저장소 Settings → Secrets and variables → Actions에 `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID`, `ADMIN_TOKEN`을 등록합니다.
3. Actions 탭에서 **Deploy edge** workflow를 수동으로 실행합니다.

## 4. 배포 후 1일차 실측

배포가 끝나면 Cloudflare 위의 runner로 baseline(A/A)과 정상 후보 trial을 한 번씩 돌립니다. 약 5~6분 걸립니다.

```bash
EDGE_URL=https://aiops-edge.<서브도메인>.workers.dev ADMIN_TOKEN=<토큰> scripts/cf-runner-smoke.sh
```

확인할 숫자는 세 가지입니다. 출력과 `.work/cf/*.json`을 Claude에게 전달하면 계획의 측정 계약을 조정합니다.

- A/A 노이즈 중앙값: 계약 허용치 15%보다 충분히 낮은지 확인합니다.
- 후보 빌드 시간(`candidate build ms`): 이미지에 미리 빌드한 의존성 덕분에 수십 초 안에 끝나야 합니다.
- 100 rps에서 누락(`dropped`)이 0인지 확인합니다.

## 5. 로컬 개발 (Docker 없이)

PostgreSQL 서버 바이너리, Rust, k6만 있으면 됩니다. root로 실행할 때는 DB 서버를 일반 사용자로 띄우도록 `--pg-user`를 줍니다.

```bash
cargo build --release -p aiops-runner
```

```bash
./target/release/aiops-runner --pg-user postgres baseline --out .work/baseline.json
```

```bash
./target/release/aiops-runner --pg-user postgres trial --patch scenarios/n-plus-one/candidates/known-good-batched-join.json --baseline-report .work/baseline.json
```

## 6. 격리 수준에 대한 메모

- runner 컨테이너는 인터넷이 차단되고, 바인딩·비밀값이 없습니다. 후보 서비스는 환경 변수를 비운 상태로 실행됩니다.
- 시나리오·golden 케이스·평가 계약은 root 소유(`/opt/aiops`)라 runner 사용자가 수정할 수 없습니다.
- 후보 서비스와 runner는 같은 사용자로 실행됩니다. 통제된 데모 코드에는 충분하지만, 임의의 고객 코드를 돌리는 단계에서는 후보마다 별도 사용자나 별도 컨테이너로 나눠야 합니다.
