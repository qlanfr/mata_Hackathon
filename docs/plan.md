# AIOps 자율 실험 엔지니어 — 해커톤 구현 계획 (Rust + React + Cloudflare)

## Context

- 목표: 업로드한 기획안 v0.1을 바탕으로 Meta Global Hackathon(마감 **2026-11-04 21:00 ET = 한국 11/5 11:00**)에 제출할 MVP를 만든다.
- 제품 정의(기획안 그대로): Meta 모델이 운영 증거를 읽고 가설과 패치를 만든다. Rust 하네스는 격리 환경에서 패치를 빌드·검증·측정한다. 판정(VERIFIED / REJECTED / INCONCLUSIVE)은 Rust 평가기만 내린다.
- 첫 시나리오: Rust 주문 목록 API의 **N+1 조회**. 목표는 정확성을 유지하면서 p95 지연을 20% 이상 개선하는 것이다.
- 기획안과 달라진 점(사용자 결정):
  - UI: Askama SSR 대신 **React SPA**.
  - 배포: Docker Compose 대신 **Cloudflare Containers 중심**.
  - 제어 DB: PostgreSQL 대신 **Cloudflare D1**. 큰 아티팩트는 **R2**에 둔다.
  - 인원: **1인 + Claude**.
  - Meta API 키: **아직 없음**. 개발 초기에는 replay provider를 쓴다.
- 저장소: `qlanfr/mata_hackathon`, 브랜치 `claude/busy-cannon-5b5n40`. 기획안이 가리키는 `qlanfr/aiops`는 참고만 한다.

### 사실 / 가정 구분

| 구분 | 내용 |
| --- | --- |
| 확인한 사실 (CF Containers 문서) | Containers와 Sandboxes는 2026-04-13 GA. 인스턴스는 최대 4 vCPU / 12 GiB / 20 GB(`standard-4`). Container 안에서 도는 코드는 activity로 치지 않는다. 그래서 요청·alarm·`renewActivityTimeout()`이 없으면 `sleepAfter`(기본 10분) 뒤에 정지된다. `enableInternet`은 Container class 기본값이 true다. `outboundByHost`로 컨테이너가 Worker 바인딩(KV·R2·D1·DO)을 호출할 수 있다. 플랫폼 종료 시 SIGTERM을 보내고 최대 15분 기다린다. |
| 확인한 사실 (CF D1 문서) | D1은 SQLite 방언이다. Worker 바인딩(또는 REST API)으로만 접근하고, TCP 접속은 없다. `batch()`는 하나의 SQL 트랜잭션이어서 한 문장이 실패하면 전부 롤백된다. 그 밖에는 auto-commit이고 대화형 트랜잭션은 없다. 결과의 `meta.changes`로 갱신된 행 수를 알 수 있다. 한도는 DB 10 GB(Paid), 문자열·BLOB·행 2 MB, 문장 100 KB, 바인딩 파라미터 100개, 쿼리 30초, 결과 50 MB(2026-11-04부터 적용). |
| 확인한 사실 (이 환경) | cargo 1.97, node 22, pnpm, psql 16, docker CLI가 있다. k6와 wrangler는 없다(npx로 사용). |
| 확인한 사실 (사용자) | Cloudflare Workers Paid 구독이 있다. |
| 미확인 (네트워크 차단) | cerebralvalley.ai와 dev.meta.ai 페이지는 열리지 않았다. Meta API 형식(Responses API, `tool_choice=auto`만 지원, `text.format` 구조화 출력)은 **기획안 서술을 그대로 따른 것**이다. 키를 받으면 첫 작업으로 검증한다. |
| 가정 | R2를 계정에서 켤 수 있다(무료 한도 안에서 충분). 배포(`wrangler deploy` 이미지 빌드)는 사용자 로컬 Docker나 GitHub Actions에서 실행한다. |

## 아키텍처

```
Browser (React SPA)
  │  GET 공개 / POST는 Bearer ADMIN_TOKEN
  ▼
Cloudflare Worker  (edge/, TypeScript ~150줄, 라우팅·인증·컨테이너 수명·바인딩 중계만 담당 — 제품 로직 없음)
  ├─ Static Assets: web/dist
  ├─ Bindings: D1 (DB) · R2 (ARTIFACTS) · Secrets (META_API_KEY, ADMIN_TOKEN)
  ├─ Cron Trigger (1분) ──► aiopsd /internal/tick  (오케스트레이터 keep-alive + 재개)
  ├─ /api/* ──► AiopsdContainer (DO, standard-1, enableInternet=true)
  │              Rust Axum: API · SSE · 상태머신 · 에이전트 하네스 · 평가기
  │              ├─ http://d1.internal/{query,batch}  → outboundByHost → env.DB
  │              ├─ http://r2.internal/<key>          → outboundByHost → env.ARTIFACTS
  │              ├─ http://runner.internal/jobs       → outboundByHost → env.RUNNER.getByName("runner-0")
  │              └─ Meta API (api.meta.ai/v1, 직접 HTTPS) ← 키 없으면 ReplayProvider
  ▼
RunnerContainer (DO, standard-4, enableInternet=false, outbound handler 없음)
   Rust aiops-runner: 작업공간 생성 · 패치 적용 · cargo build --offline
   · 내장 PostgreSQL(실험 대상 DB, seed 미리 적재) · 정확성 검사 · k6 부하 · 프로세스 그룹 정리
```

**DB가 두 종류인 이유**: 제어 DB(실행 기록·작업 큐)는 D1이다. 실험 대상 DB는 runner 안의 PostgreSQL을 그대로 쓴다. 제품이 겨냥하는 사용자가 "API + PostgreSQL"을 운영하는 팀이어서, 최적화 대상까지 D1로 바꾸면 데모 시나리오가 실제 사용자 환경과 달라진다.

### 핵심 설계 선택과 이유

1. **runner는 인터넷을 차단하고 바인딩도 주지 않는다.** 의존성은 이미지에 vendoring해 `CARGO_NET_OFFLINE=true`로 빌드한다. RunnerContainer 클래스에는 outbound handler가 없어서 D1과 R2에도 닿지 못한다. 결과는 aiopsd가 polling 응답으로 가져가 저장한다. 그래서 runner에는 비밀값이 하나도 없다(기획안 7.3).
2. **컨테이너에서 바인딩으로 가는 통신은 모두 outbound handler를 거친다.** AiopsdContainer의 `outboundByHost`가 세 호스트를 받는다.
   - `runner.internal`: Runner DO stub의 fetch로 넘긴다. aiopsd는 작업 중 `GET /jobs/{id}`를 5초마다 polling한다. 이 polling이 상태 확인이면서 runner keep-alive 역할도 한다.
   - `d1.internal`: `POST /query {sql, params}`와 `POST /batch [{sql, params}]`를 받아 `env.DB.prepare().bind().run()` 또는 `env.DB.batch()`로 실행하고 `{results, meta}`를 돌려준다.
   - `r2.internal`: `PUT/GET /<key>`를 `env.ARTIFACTS`로 넘긴다.
3. **D1 위에서 트랜잭션은 CAS와 batch로 대신한다.** 대화형 트랜잭션이 없기 때문이다.
   - 상태 전이: `UPDATE runs SET state=?, state_version=state_version+1 WHERE id=? AND state_version=?`를 실행하고 `meta.changes == 1`인지 확인한다.
   - 상태 변경과 이벤트 기록처럼 함께 바뀌어야 하는 문장은 하나의 `batch()`로 묶는다. batch는 하나의 트랜잭션이다.
   - `run_events.seq`는 `INSERT ... SELECT COALESCE(MAX(seq),0)+1 FROM run_events WHERE run_id=?` 한 문장으로 매긴다.
   - 오케스트레이터는 인스턴스가 하나뿐이다. 그래서 `SKIP LOCKED` 없이 CAS lease로 충분하다.
4. **keep-alive와 재개는 Cron이 맡는다.** 브라우저가 닫히면 aiopsd에 요청이 끊긴다. 그래서 Worker Cron이 1분마다 `/internal/tick`을 호출한다. tick은 D1에서 종료되지 않은 run을 찾아 한 단계씩 진행한다. 이렇게 하면 재시작해도 이어서 진행된다(기획안 6.2).
5. **runner 재시작은 boot_id로 감지한다.** runner는 응답마다 `boot_id`를 실어 보낸다. 바뀌면 해당 attempt를 `RUNNER_LOST`로 기록하고, 아티팩트를 대조한 뒤에만 재시도한다. 같은 작업을 확인 없이 다시 실행하지 않는다.
6. **패치는 search/replace 블록으로 받는다.** 모델이 unified diff를 정확히 만들어 내는 경우가 드물다. 그래서 `{path, search, replace}[]` 형식으로 받는다. runner는 search 문자열이 정확히 1회 일치하는지 확인한 뒤 적용한다. 화면에 보여 줄 diff는 runner가 `git diff`로 생성한다. 허용 경로는 `fixtures/orders-api/src/**`이고 `tests`, `Cargo.*`는 제외한다. 경로 정규화와 symlink 검사를 거친다.
7. **provider는 trait 하나로 추상화한다.** `LlmProvider` 구현은 두 가지다. `MetaProvider`는 reqwest로 Responses API를 호출한다. `ReplayProvider`는 `prompts/replay/n-plus-one/*.json`의 스크립트 응답을 쓴다. UI에 provider 배지를 표시해 replay 실행을 실제 모델 실행처럼 보이지 않게 한다.
8. **trial 실행은 상태머신이 주도한다.** 모델은 계획과 패치만 제안한다. `request_trial` 도구는 쓰지 않고, 오케스트레이터가 정책·예산을 검사한 뒤 job을 만든다. 1인 개발에 맞춰 결정성과 디버깅 용이성을 우선한 선택이다.
9. **DB 접근은 trait 하나, 구현은 둘이다.** `ControlDb` trait의 구현은 다음 두 가지다.
   - `D1Db`: 운영용. reqwest로 `d1.internal` 호출.
   - `SqliteDb`: 로컬 개발·테스트용. sqlx-sqlite 파일 또는 in-memory.
   - 둘 다 SQLite 방언이라 같은 SQL과 같은 `migrations/*.sql`을 쓴다. 운영에는 `wrangler d1 migrations apply`로, 로컬에는 같은 파일을 직접 적용한다.

## 저장소 구조 (신규)

```
Cargo.toml                 # workspace: crates/*, apps/*  (fixtures는 exclude)
crates/aiops-core/         # domain 타입, RunState 상태머신, Policy, Budget, ScenarioSpec,
                           # LlmProvider trait + Meta/Replay, 에이전트 루프, Evaluator(판정 규칙)
apps/aiopsd/               # Axum API, SSE, ControlDb(D1Db·SqliteDb), orchestrator tick, runner client
apps/aiops-runner/         # Axum job API, workspace/patch, build·check·bench, cleanup(pgid kill)
fixtures/orders-api/       # 개선 대상: Axum+SQLx(Postgres, 런타임 쿼리), N+1 포함, 요청별 JSON 로그
scenarios/n-plus-one/      # scenario.toml(허용경로·예산·계약), seed.sql, k6.js, cases/*.json(골든)
migrations/                # 제어 DB 스키마(SQLite/D1). wrangler d1 migrations와 로컬이 같은 파일 사용
prompts/                   # observer.md, experimenter.md, schemas/*.json, replay/
web/                       # React + Vite + TS, TanStack Query, react-router, EventSource
edge/                      # wrangler.jsonc(D1·R2·containers·cron), src/index.ts (Worker + 2 Container 클래스)
deploy/                    # Dockerfile.aiopsd, Dockerfile.runner, docker-compose.yml(로컬)
docs/                      # architecture.md(mermaid), decisions.md, demo-script.md
```

## 데이터·API (기획안 10장 축소판)

- 테이블(D1/SQLite):
  - `runs`: fingerprint, target_sha, scenario_hash, state, state_version, budget, cancel_requested, provider.
  - `trials`: hypothesis, plan, patch_hash, verdict, reason.
  - `jobs`: kind, lease_until, runner_boot_id, attempt.
  - `run_events`: seq, kind, payload. 모델 사용량·도구 호출·상태 변경을 담는다.
  - `artifacts`: kind, sha256, size, r2_key, inline_json. 64 KB 이하의 요약 JSON은 inline으로 두고, 로그·raw k6 출력·diff는 R2에 둔다.
  - `experiment_cases`: 조건, 결과, 검증 수준.
- 타입 규칙: id는 TEXT(UUIDv7), 시각은 INTEGER(epoch ms), JSON은 TEXT로 두고 SQLite JSON 함수로 조회한다.
- `incidents` 테이블은 따로 두지 않는다. `runs(fingerprint) WHERE state NOT IN (종료 상태)`에 부분 unique index를 걸어 중복 알림을 병합한다. SQLite도 부분 인덱스를 지원한다.
- API는 기획안의 7개 그대로다: `POST /alerts`, `POST /runs`, `GET /runs/{id}`, `GET /runs/{id}/events`(SSE + `?after_seq=` polling 겸용), `POST /runs/{id}/cancel`, `GET /trials/{id}`, `GET /artifacts/{id}`. 여기에 목록용 `GET /runs`를 추가한다.
- SSE는 프로세스 안의 broadcast로 실시간 이벤트를 보낸다. 재연결 시 놓친 구간은 D1에서 `after_seq`로 읽는다.

## 평가 계약 (scenario.toml에 고정, 모델 수정 불가)

- 정확성: 골든 케이스를 모두 통과해야 한다(금액·수량·합계, 정렬·페이지네이션, 빈 목록, 타 사용자 배제, 오류 의미).
- 성능: k6 `constant-arrival-rate`로 부하를 준다. 1회 측정은 warmup 5초 + 측정 20초다. baseline과 candidate를 번갈아 **3쌍** 측정한다. 쌍별 개선율 `(b−c)/b`의 중앙값이 20% 이상이고 모든 쌍의 개선 방향이 같으면 VERIFIED다.
- 판정 게이트:
  - run 시작 시 A/A 변동이 허용 폭을 넘으면 INCONCLUSIVE.
  - `dropped_iterations` > 0이거나 drain 미충족이면 INCONCLUSIVE.
  - 오류·timeout 회귀, OOM이 나면 REJECTED.
  - 빌드 실패는 판정이 아니라 실행 오류로 기록한다.
- p95는 `http_req_duration{expected_response:true}` submetric으로 계산하고, 실패 요청은 별도 게이트로 판정한다.

## 일정 (1인, 오늘 11/2 KST → 11/5 11:00 KST)

| 날짜 | 할 일 | 완료 기준 |
| --- | --- | --- |
| **1일차 · 11/2** | workspace 골격; fixture API(N+1) + seed + 골든 케이스 + k6; aiops-runner 로컬 실행(내장 PG, build, check, bench, cleanup); Evaluator; 수동 후보 2개(정상 JOIN 버전, 필드 누락 버전). **CF 탐색**: D1·R2 생성, runner 이미지를 Container에 올려 cargo 증분 빌드 시간과 A/A 변동을 실측, 컨테이너→`d1.internal` 왕복 지연 측정 | 로컬에서 정상 후보 → VERIFIED, 누락 후보 → REJECTED. CF에서 runner 1회 왕복 |
| **2일차 · 11/3** | migrations(SQLite) + ControlDb(D1/SQLite) + 상태머신/tick/jobs/lease; runner client(outbound 경유); run_events + SSE; LlmProvider(Replay 먼저, 키가 오면 Meta); 관측·실험 두 역할 + 도구(read_request_evidence, read_source, search_experiments, submit_patch, read_trial_result); 예산·취소; edge Worker(D1·R2 중계) + cron 배포 | CF 배포본에서 `POST /runs` → replay(또는 Meta) 제안 → runner 검증 → 판정까지 완주 |
| **3일차 · 11/4** | React 3화면(실행 목록 / 상세 타임라인 / 결과 비교·diff); 복구·취소·중복 알림 검증; experiment_cases 기록; README·재현 가이드·데모 녹화 | 다른 사람이 README대로 재현 가능, 데모 영상 완성 |
| 11/5 오전 | 버퍼 + 제출 | 11:00 전 제출 |

2일차가 끝날 때 완주하지 못하면 다음 순서로 범위를 줄인다: `POST /alerts` → experiment_cases 검색 → UI 장식 → 독립 확인 조건.

## 1인 개발을 위한 범위 축소 (기획안 P0 대비)

- Prometheus/Grafana는 P1로 미룬다. 관측 증거는 fixture가 남기는 요청별 JSON 로그(request_id, sql_count, db_ms, latency_ms, status)와 k6 summary로 대체한다.
- 벡터 검색(Vectorize 또는 pgvector), OpenTelemetry, GitHub PR 생성은 P1로 미룬다.
- 최종 독립 확인(별도 seed 1쌍)은 시간이 남을 때만 한다.

## 리스크와 반례

| 리스크 | 대응 |
| --- | --- |
| CF Container 측정 잡음이 커서 20% 판정이 흔들림 | N+1 → JOIN은 보통 수 배 차이가 나서 여유가 크다(1일차 로컬 실측: 약 10배 차이, 아래 진행 상황 참고). A/A 게이트로 INCONCLUSIVE 처리. 그래도 불안정하면 부하·데이터 크기를 키운다 |
| standard-4에서 cargo 증분 빌드가 너무 느림 | 이미지에 baseline release 빌드와 `target/`을 미리 굽는다. 그래도 느리면 `opt-level=1` 프로필을 시나리오 계약에 고정한다 |
| D1에 대화형 트랜잭션이 없음 | CAS UPDATE + `meta.changes` 확인, 함께 바뀌어야 하는 문장은 `batch()`(하나의 트랜잭션)로 묶는다. 장시간 작업 중에는 DB 잠금을 잡지 않는다는 기획안 원칙과도 맞는다 |
| D1 행 2 MB 한도 | 큰 로그·raw 측정·diff는 R2에 두고 D1에는 해시·크기·키만 둔다. 모델 컨텍스트로 보내는 증거도 Rust에서 잘라서 보낸다 |
| 컨테이너→Worker→D1 왕복 지연 | 한 tick의 쿼리를 batch로 묶는다. Container 배치 지역(`constraints.regions`)과 D1 location hint를 같은 지역(예: WNAM)으로 맞춘다. 실험 측정 경로에는 D1이 끼지 않으므로 판정에는 영향이 없다 |
| Worker → Container SSE 스트리밍 문제 | events API를 `after_seq` polling과 같은 커서로 설계해 두고, 문제가 생기면 프론트를 2초 polling으로 전환한다 |
| Meta API 키가 늦게 옴 / 형식이 기획안과 다름 | provider trait 뒤에 숨겨 둔다. 키를 받은 날 계약 테스트(JSON 출력, 도구 왕복, 사용량)를 먼저 돌린다. 끝까지 키가 없으면 replay로 데모하되 UI와 영상에 "REPLAY"를 명시한다 |
| 로컬 Docker 없이 배포 불가 | GitHub Actions에서 `wrangler deploy`를 실행하는 워크플로를 둔다(CF API 토큰은 사용자가 secret으로 등록) |
| 모델 패치가 search 불일치나 범위 밖 경로를 가리킴 | policy 거절을 run_event로 남기고, 구조화 응답 보정 1회 후 다음 턴으로 넘긴다(예산 차감) |

## 진행 상황

### 1일차 (11/2) — 로컬 완료, Cloudflare 실측은 배포 대기

로컬 측정 환경: Claude Code 클라우드 컨테이너(4 vCPU), rustc 1.97, PostgreSQL 16, k6 v0.57.0, 100 rps, 20초 × 3쌍.
Cloudflare Container에서 잰 값이 아니며, 같은 숫자가 나온다고 가정하지 않는다.

| 확인 항목 | 결과 |
| --- | --- |
| A/A 노이즈 (baseline 대 baseline) | 쌍별 0.7% / 3.7% / 2.3%, 중앙값 2.3% (허용치 15%) |
| 관측 증거 | 요청당 SQL 평균 81.9개(최대 182), 평균 DB 시간 28.5 ms |
| 정상 후보 (JOIN 1회) | **VERIFIED** — p95 48~51 ms → 4.1~4.9 ms, 쌍별 개선 89.9% / 91.5% / 92.0%, 요청당 SQL 3.0개, golden 11/11 |
| 항목 누락 후보 | **REJECTED** — golden 6/11, 부하 측정 없이 9초 만에 종료 |
| `Cargo.toml` 수정 후보 | **POLICY_REJECTED** — 아무것도 실행하지 않음 |
| 1,500 rps 과부하 | **INCONCLUSIVE** — baseline이 반복 6,332건 누락 |
| 측정 중 취소 (HTTP API) | `cancelled`, 남은 프로세스 0 |
| 후보 증분 빌드 | 8.1초 (`--offline --locked`) |

남은 1일차 항목은 사용자의 배포(`docs/setup.md`)가 필요하다: Cloudflare Container에서 빌드 시간·A/A 노이즈·누락 여부 실측.
위 후보 세 개는 하네스 검증용으로 직접 작성한 것이며 모델 출력이 아니다.

### 2일차 (11/3) — 로컬 완료(Replay), Cloudflare 배포 대기

만든 것: `aiopsd`(상태 머신, D1/SQLite 제어 DB, 에이전트 하네스와 도구 6종, Meta/Replay provider, API·SSE, 알림 병합, 취소), edge Worker 확장(aiopsd 컨테이너, `runner`/`d1`/`r2` relay, cron), `Dockerfile.aiopsd`, D1 `aiops-control`(WNAM)과 R2 `aiops-artifacts` 생성.

로컬 측정 환경은 1일차와 같다(4 vCPU 개발 컨테이너, 100 rps). 모델은 Replay provider이며, Meta API는 아직 호출하지 않았다.

| 확인 항목 | 결과 |
| --- | --- |
| `POST /api/v1/runs` 한 번으로 완주 | **COMPLETED** (290초): baseline(A/A 6.5%) → 관측 AI(도구 3회) → 실험 AI(도구 2회) → 사전 검사 → 빌드 10.4초 → golden 11/11 → **VERIFIED**, 쌍별 p95 개선 88.9 / 91.3 / 90.7% |
| 측정 중 다른 작업으로 CPU 간섭 | 같은 패치가 **INCONCLUSIVE** (baseline p95 쌍별 편차 19.7% > 15%). 잘못된 VERIFIED는 나오지 않았다. 이후 INCONCLUSIVE는 같은 패치를 1회 재측정하도록 바꿨다 |
| 같은 fingerprint 알림 2회 | 두 번째는 같은 run에 `alert_merged`로 병합. run이 끝난 뒤 같은 fingerprint는 새 run 생성 |
| baseline 측정 중 취소 | CANCELLING → CANCELLED 5.5초, "runner가 프로세스 종료를 확인", 남은 프로세스 0 |
| aiopsd 강제 종료(SIGKILL) 후 재시작 | 같은 runner job을 이어 받아 진행, job 중복 없음 |
| trial 측정 중 runner 재시작(SIGTERM → 새 boot_id) | job 유실 기록 → 새 runner에 2차 시도 → COMPLETED(VERIFIED) |
| runner가 꺼진 동안 취소 | CANCELLING 유지 → runner가 새 boot_id로 돌아오자 "runner 재시작으로 프로세스 소멸"로 CANCELLED |
| 로컬 D1(workerd)에서 마이그레이션·핵심 SQL | 부분 unique index, 조건부 이벤트 INSERT, CAS 동작 확인. 원격 D1에서는 배포 때 처음 실행된다 |

검증 중 찾아서 고친 문제:
- 정확성 검사처럼 polling 간격(3초)보다 짧은 단계가 타임라인에서 빠졌다 → runner가 단계 이력을 돌려주고 aiopsd가 순서대로 기록.
- 종료 중인 runner가 새 job을 받아 재시도가 사라졌다 → 종료 중에는 503으로 거절.
- runner에 닿지 못하는 동안 같은 오류 이벤트가 3초마다 쌓였다 → 같은 오류는 한 번만 기록.
- 보류(INCONCLUSIVE) 판정에서도 쌍별 개선율을 기록하도록 평가기 수정.

남은 것: 사용자 배포 후 Cloudflare에서 같은 시나리오 실행, Meta API 키를 받은 뒤 계약 테스트.

### 3일차 (11/4) — 화면 완료(로컬), 배포·영상 대기

만든 것: React 화면 3개(실행 목록, 실행 상세 실시간 타임라인, 결과 페이지), Worker 정적 자산 연결(`/api/*`·`/runner/*`·`/healthz`만 Worker 먼저, 나머지는 SPA), 배포 스크립트(`npm run deploy`가 화면 빌드 포함), 데모 스크립트, 제출 체크리스트.

| 확인 항목 | 결과 |
| --- | --- |
| 세 화면, 데스크톱(라이트)·폰(다크) | 가로 넘침 0, 브라우저 오류 0 |
| 화면에서 run 시작 → 실시간 이벤트 → 취소 | 5.5초에 첫 상태 이벤트, 11.4초에 CANCELLED(runner 정리 확인), 남은 프로세스 0 |
| `wrangler dev`(컨테이너 제외) 라우팅 | `/`·딥링크는 index.html, 토큰 없거나 틀린 쓰기·`/runner/*`는 401, `/healthz`는 Worker |
| `wrangler deploy --dry-run` | Worker 번들, 정적 파일 4개, 바인딩 6개, 컨테이너 2개 인식 |

검증 중 고친 것: 폰에서 p95 차트 글자가 너무 작아지던 문제(컨테이너 실제 폭으로 다시 그림), 해시를 끝자리로 줄여 화면마다 다르게 보이던 문제(해시는 앞자리, ID는 뒷자리).

### 배포 준비 — 컨테이너 이미지 검증 완료, Cloudflare 배포는 secret 등록 대기

Claude의 개발 환경은 네트워크 정책 때문에 Cloudflare API·registry·`workers.dev`와 Debian 미러에 접속할 수 없다. 그래서 이미지 빌드와 배포는 GitHub Actions에서 한다.

GitHub Actions **Container E2E** ([run 41413453840](https://github.com/qlanfr/mata_Hackathon/actions/runs/41413453840), ubuntu-latest, Replay provider)

| 확인 항목 | 결과 |
| --- | --- |
| 이미지 빌드 | 둘 다 성공. runner 1.9 GB(약 140초), aiopsd 95.8 MB(115초) |
| runner 격리 | 내부 전용 Docker 네트워크에서 외부 HTTPS 접속 실패를 확인 |
| 전체 run | **COMPLETED** 301초. A/A 1.6%, 후보 빌드 6.1초, golden 11/11, **VERIFIED**(쌍별 p95 개선 90.3 / 90.5 / 90.2%) |

같은 날 로컬(4 vCPU 개발 컨테이너)에서 `scripts/drive-run.sh`로 돌린 run에서는 첫 측정이 baseline 편차 68%로 보류됐다. 이어서 재측정 1회에서 VERIFIED(92.3%)가 나왔다. 보류 후 재측정 경로가 실제로 동작함을 보여 준다.

이 검증은 Worker relay(`d1.internal`, `r2.internal`, `runner.internal`), cron, Cloudflare 컨테이너 시작 지연을 다루지 않는다. Deploy edge 워크플로는 배포 직후 배포된 URL에서 같은 run을 1회 돌린다.

## 검증 방법

- `cargo test -p aiops-core`: 상태 전이 표, 예산 초과 거절, policy(범위 밖 경로·symlink) 거절, Evaluator 규칙(중앙값·방향 일치·게이트) 단위 테스트.
- `cargo test -p aiopsd`: in-memory SQLite에 `migrations/*.sql`을 적용하고 CAS 경합(같은 `state_version`으로 두 번 갱신 → 한 번만 성공), batch 원자성, `run_events.seq` 연속성, 중복 알림 병합을 확인한다.
- `docker compose -f deploy/docker-compose.yml up` 후 스크립트로 정상 후보 → VERIFIED, 필드 누락 후보 → REJECTED, k6 rate 과다(dropped) → INCONCLUSIVE를 재현한다.
- ReplayProvider로 `POST /runs` E2E: run_events 순서·상태 최종값·artifact 해시를 확인한다.
- 취소: 실행 중 `POST /cancel` → runner 프로세스 그룹 종료 확인(`ps`) → CANCELLED.
- 재시작: aiopsd를 재기동한 뒤 tick이 이어서 진행하는지, runner를 재기동하면 boot_id 불일치가 감지되는지 확인한다.
- CF 배포본: `wrangler d1 migrations apply --remote` 후 같은 E2E를 1회 이상 수행하고, run ID·baseline SHA·원본 측정을 README에 링크한다. runner 컨테이너에서 `d1.internal` 호출이 실패하는지(접근 불가)도 확인한다.
