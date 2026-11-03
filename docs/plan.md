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

### 배포 준비 — 컨테이너 이미지 검증

Claude의 개발 환경은 네트워크 정책 때문에 Cloudflare API·registry·`workers.dev`와 Debian 미러에 접속할 수 없다. 그래서 이미지 빌드와 배포는 GitHub Actions에서 한다.

GitHub Actions **Container E2E** ([run 41413453840](https://github.com/qlanfr/mata_Hackathon/actions/runs/41413453840), ubuntu-latest, Replay provider)

| 확인 항목 | 결과 |
| --- | --- |
| 이미지 빌드 | 둘 다 성공. runner 1.9 GB(약 140초), aiopsd 95.8 MB(115초) |
| runner 격리 | 내부 전용 Docker 네트워크에서 외부 HTTPS 접속 실패를 확인 |
| 전체 run | **COMPLETED** 301초. A/A 1.6%, 후보 빌드 6.1초, golden 11/11, **VERIFIED**(쌍별 p95 개선 90.3 / 90.5 / 90.2%) |

같은 날 로컬(4 vCPU 개발 컨테이너)에서 `scripts/drive-run.sh`로 돌린 run에서는 첫 측정이 baseline 편차 68%로 보류됐다. 이어서 재측정 1회에서 VERIFIED(92.3%)가 나왔다. 보류 후 재측정 경로가 실제로 동작함을 보여 준다.

이 검증은 Worker relay(`d1.internal`, `r2.internal`, `runner.internal`), cron, Cloudflare 컨테이너 시작 지연을 다루지 않는다. Deploy edge 워크플로는 배포 직후 배포된 URL에서 같은 run을 1회 돌린다.

### Cloudflare 배포 (11/2) — 배포본에서 전체 run VERIFIED

- 배포: GitHub Actions Deploy edge [run 41466335890](https://github.com/qlanfr/mata_Hackathon/actions/runs/41466335890). 이미지 두 개 빌드·푸시와 Worker 배포에 4분 43초가 걸렸다.
- URL: https://aiops-edge.yh9365.workers.dev
- run `01a19cc5-b13c-7384-91e7-6a1ebcafeb02`(Replay provider): 생성부터 COMPLETED까지 6분 8초.

| 확인 항목 | 결과 |
| --- | --- |
| aiopsd 컨테이너 시작 | 배포 14초 뒤 `/api/v1/runs`가 응답. run 생성이 D1에 기록됨(`d1.internal` relay 동작) |
| runner 컨테이너 첫 시작 | 첫 job 제출부터 접수까지 약 70초(503 → timeout → 접수). `runner.internal` relay 동작 |
| 측정 환경 | 4 vCPU, PostgreSQL 15.19, k6 v0.57.0 |
| A/A 노이즈 | 쌍별 p95 20.0/18.7, 18.6/17.6, 17.2/18.5 ms. 중앙값 6.4%(허용치 15%), 누락 0 |
| 후보 증분 빌드 | 4.5초 (`--offline`, 인터넷 차단 컨테이너) |
| 판정 | **VERIFIED**: golden 11/11. p95 17.0~17.8 ms → 1.87~2.01 ms, 쌍별 개선 88.7 / 89.2 / 89.0% |

검증 중 고친 것:
- 첫 배포 시도에서 D1 요청이 인증 오류 10000으로 실패했다. `CLOUDFLARE_ACCOUNT_ID` secret 값이 토큰의 계정과 달랐다. 워크플로가 토큰이 접근할 수 있는 계정 목록과 secret을 대조해 계정을 고르도록 바꿨다.
- run은 성공했지만 이벤트 수집에서 workers.dev가 Python urllib 요청을 403으로 막았다. 이벤트 수집도 curl로 바꿨다.
- 두 번째 배포 직후의 run이 FAILED("no runner job is active in state BASELINING")로 끝났다.
  - 원인: 롤아웃 중 옛 aiopsd 컨테이너가 SIGTERM을 받고도 계속 돌았다. PID 1이라 처리기가 없는 SIGTERM을 무시했다.
  - 그 결과 옛 aiopsd와 새 aiopsd가 같은 run을 동시에 tick했다. 한쪽이 baseline job을 `done`으로 바꾼 지 11 ms 뒤, 다른 쪽이 활성 job이 없다고 보고 run을 실패시켰다.
  - 수정 1: D1 `leases` 테이블(마이그레이션 0002). 리스를 가진 인스턴스 하나만 tick하고, heartbeat로 리스를 갱신한다(TTL 60초).
  - 수정 2: aiopsd가 SIGTERM을 받으면 진행 중인 tick을 마치고 리스를 넘긴 뒤 종료한다.
  - 로컬 재현: aiopsd 두 개를 같은 DB에 띄우고 baseline 중에 리스 보유자를 SIGTERM으로 내렸다. 2 ms 만에 리스를 반납했고, 다른 인스턴스가 2.5초 뒤 이어받아 COMPLETED/VERIFIED로 끝냈다. 중복 이벤트는 없었다.
  - 리스를 배포하던 롤아웃에서도 옛 컨테이너(수정 전 코드)와 tick이 겹쳤다. 같은 job이 거의 동시에 두 번 제출됐고("runner busy"), PREPARING→BUILDING 이벤트가 두 번 기록됐다. run 자체는 COMPLETED/VERIFIED(89.8%)로 끝났다.
  - 이 겹침에서 드러난 것: CAS에서 진 배치도 뒤따르는 문장(job, 이벤트)을 써 버렸다. 수정 3: `cas_guard` 테이블(마이그레이션 0003). 전환 배치가 UPDATE 직후 `changes()`를 기록하고, 0이면 CHECK 위반으로 배치 전체가 롤백된다. 로컬 D1(workerd)에서 `changes()`와 오류 문구("CHECK constraint failed: cas_lost")를 확인했고, 단위 테스트를 추가했다.
  - 두 수정을 배포한 뒤의 롤아웃([Deploy edge run 41473940043](https://github.com/qlanfr/mata_Hackathon/actions/runs/41473940043), 워크플로 전체 성공): 실행 중 aiopsd와 runner가 모두 교체됐다. 중복 제출이나 중복 이벤트 없이 COMPLETED/VERIFIED로 끝났다(A/A 1.3%, 쌍별 개선 89.1 / 88.7 / 88.6%, 상태 이벤트 9개로 첫 run과 같음).
  - 같은 롤아웃에서 runner도 교체됐다. 진행 중이던 job은 "runner cancelled the job unexpectedly"로 유실 처리됐고, 새 runner에서 2차 시도로 이어졌다. 이 경로는 원래 설계대로 동작했다.

확인하지 못한 것:
- R2 경로: 아티팩트 두 개가 모두 64 KB 미만이라 D1에 inline으로 저장됐다.
- Worker cron tick: 이번 run은 aiopsd 내부 tick으로 진행됐고, cron이 tick을 보냈는지는 확인하지 않았다.
- Meta API 실호출.

### 하네스 함정 모음 (11/3) — 14개 후보, 기대 판정과 모두 일치

평가기의 관문마다 그 관문에서만 걸리는 함정 패치를 직접 작성했다. 모두 하네스 검증용이며 모델 출력이 아니다.
- 기대 판정: `scenarios/n-plus-one/candidates/suite.json`
- 실행: `scripts/harness-suite.sh`(로컬 바이너리 또는 `RUNNER_IMAGE`로 runner 이미지 안에서)
- CI: **Harness suite** 워크플로가 push마다 runner 이미지(네트워크 차단) 안에서 다시 돌린다.

로컬 결과(4 vCPU 개발 컨테이너, 100 rps, 20초 × 3쌍):

| 후보 | 관문 | 판정 | 근거 |
| --- | --- | --- | --- |
| known-good-batched-join (정상 JOIN) | 개선 | VERIFIED | 쌍별 p95 개선 중앙값 90.5% |
| known-bad-drops-items (항목 누락) | 정확성 | REJECTED | golden 6/12 |
| known-policy-violation (`Cargo.toml`) | 정책 | POLICY_REJECTED | 보호 파일 |
| trap-items-in-product-order (항목을 상품 ID순으로 정렬) | 정확성 | REJECTED | golden 11/12: 새 케이스만 실패 |
| trap-page-shifted (offset을 1부터 셈) | 정확성 | REJECTED | golden 6/12 |
| trap-other-users-orders (`>=` 오타로 남의 주문 섞임) | 정확성 | REJECTED | golden 5/12 |
| trap-totals-rounded (금액을 달러 단위로 반올림) | 정확성 | REJECTED | golden 6/12 |
| trap-skips-user-check (사용자 확인 생략, 404 → 빈 200) | 정확성 | REJECTED | golden 11/12: `unknown-user` |
| trap-edits-tracker (SQL 계측을 꺼서 지표 조작) | 정책 | POLICY_REJECTED | `src/tracker.rs` 보호 |
| trap-edits-golden-answers (`../`로 정답 파일 수정) | 정책 | POLICY_REJECTED | 상대 경로 탈출 거부 |
| trap-no-measurable-effect (`with_capacity` 제거) | 개선 | INCONCLUSIVE | 쌍별 4.6 / −1.9 / −11.2%, 방향 불일치 |
| trap-slower (상품마다 쿼리 1회 추가) | 개선 | REJECTED | p95 −189.8%(약 2.9배 느림) |
| trap-crashes-on-last-product (1,000칸 캐시 off-by-one) | 오류율 | REJECTED | 오류율 0.30% vs 0.00% |
| trap-does-not-compile (`Vec<i32>` 타입 오류) | 빌드 | EXECUTION_ERROR | 빌드 실패 |

빌드 버그를 고친 뒤 판정 이유까지 대조하며 차가운 시작으로 다시 돌렸다. 14개 모두 기대 범위 안이었다.
- 13개는 판정과 이유가 모두 일치했다.
- 효과 없는 변경은 이번에 "개선 5.8% < 20%"로 REJECTED였다(허용 범위).
- 정상 후보 91.1%, 느린 후보 p95 −395%, 크래시 함정은 오류율 0.30%.
- CI([Harness suite run 41486698934](https://github.com/qlanfr/mata_Hackathon/actions/runs/41486698934)): runner 이미지 안, 네트워크 차단, 후보마다 새 컨테이너로 돌렸다. 13개 PASS, 1개 허용, 실패 0이다. 효과 없는 변경은 "−2.1% 느려짐"으로 REJECTED였고, 정상 후보는 91.4%였다.

함정을 만들다가 golden의 빈틈을 하나 찾았다.
- 기존 golden 페이지의 주문들은 모두 항목 순서와 상품 ID 순서가 우연히 같았다.
- 그래서 "JOIN으로 바꾸면서 상품 ID로 정렬"한 오답이 golden 11/11을 통과했고, **VERIFIED(개선 91.4%)를 받았다.**
- seed를 시뮬레이션해 보면 순서가 다른 주문은 6만 개 중 2,040개다. 부하 측정 사용자 2,000명 중 68명의 첫 페이지에 이런 주문이 있다.
- 순서가 모두 섞인 user 82의 페이지를 golden 케이스(`items-not-in-product-order`)로 추가했다. 이제 이 오답은 REJECTED된다. 기존 golden 11개의 정답 파일은 재생성 후에도 바뀌지 않았다.

12개 세트를 배포하면서 재현성 기록의 빈틈도 드러났다.
- 롤아웃 중 옛 aiopsd가 run을 만들었고, 측정은 새 runner가 했다.
- 그 결과 run에는 옛 시나리오 해시(`dec57cc5404d`, 11개)가 기록됐고, runner 보고서에는 새 해시(`939f0d479f38`, 12개)가 찍혔다. 판정은 VERIFIED였다.
- 수정: aiopsd가 runner 보고서의 시나리오 해시를 run의 해시와 대조한다. 다르면 "scenario changed during the run"으로 run을 멈춘다.
- `scripts/drive-run.sh`는 이 경우와 "모든 trial이 INCONCLUSIVE라 EXHAUSTED"인 경우에만 새 run을 한 번 더 시작한다. 두 번째 경우는 GitHub 러너에서 trial 내 baseline 편차가 28%와 46%로 나와 Container E2E가 실패했던 일 때문이다. 이때 잘못된 VERIFIED는 없었다.

CI에서 함정 모음을 처음 돌렸을 때 runner의 빌드 버그가 드러났다. CI는 후보마다 새 컨테이너에서 돌린다.
- 증상: 컴파일되지 않는 패치가 빌드에 성공했다. 항목을 빼는 패치가 golden을 통과했다. 모든 후보의 개선율이 ±2%였다.
- 원인:
  - 원본 바이너리가 캐시되지 않은 runner에서 trial은 후보 복사·패치 → 원본 빌드 → 후보 빌드 순으로 진행한다.
  - 모든 빌드가 cargo target 디렉터리 하나를 공유한다. 최근 cargo는 경로 패키지의 fingerprint를 작업 공간 기준 상대 경로로 계산하므로, 두 사본의 fingerprint가 같아진다.
  - 후보 소스가 방금 끝난 원본 빌드보다 오래돼 보여 cargo가 빌드를 건너뛰었다. 그래서 원본 바이너리가 후보로 복사됐다.
  - 원본이 이미 캐시된 runner(로컬, Cloudflare)에서는 드러나지 않았다. Cloudflare에서도 실행 중 runner가 재시작되면 생길 수 있었다.
  - 후보가 원본과 같아질 뿐이라 개선을 숨기는 쪽(거짓 음성)으로만 작용하고, 없는 개선을 만들지는 못한다.
- 수정: 빌드 직전에 후보 소스의 수정 시각을 현재로 찍는다. 같은 조건(원본 미캐시)을 로컬에서 재현해 수정 전에는 빌드가 0.25초 만에 "성공"하고, 수정 후에는 E0277로 실패함을 확인했다.
- 함정 모음 강화:
  - 판정 이유도 대조한다. CI에서는 크래시 함정이 오류율이 아니라 "개선 부족"으로 기각되는 등 맞는 판정을 틀린 이유로 받은 경우가 있었다.
  - 로컬 실행도 trial마다 새 작업 디렉터리(차가운 시작)로 돌린다.

오류율 함정이 golden을 통과하는 이유: 상품 1000은 golden 페이지에 한 번도 나오지 않는다. 부하 측정에서는 2,000명 중 6명의 첫 페이지에 나오며, seed 시뮬레이션으로 미리 계산했다. 측정된 오류율 0.30%는 이 6/2,000과 일치한다.

### Meta Model API 연결 (11/3) — muse-spark-1.3이 Cloudflare에서 VERIFIED

- **계약 테스트:** `aiopsd --contract-test`를 Meta contract 워크플로에서 돌렸다. 이 개발 환경에서는 `api.meta.ai`에 닿지 않아 GitHub Actions에서 실행한다.
  - 우리 Responses API 요청 형식(`POST /v1/responses`, function tool, `function_call_output`, `text.format` json_schema strict)이 수정 없이 받아들여졌다.
  - 키로 쓸 수 있는 모델(`GET /v1/models`): `muse-wonder-1.0`, `muse-spark-1.3`, `muse-spark-1.3-contributor`, `muse-spark-1.2`, `muse-spark-1.2-contributor`, `muse-spark-1.1`, `sam-3.1`, `muse-voice-transcribe-1.0`, `muse-image-1.0`.
  - `muse-spark-1.3`과 `muse-wonder-1.0` 모두 도구 호출 왕복과 JSON 스키마 답변을 통과했다(각 약 4초, 10초).
- **고친 것:**
  - 모델은 도구를 부르기 전에 한 줄 설명을 함께 보낸다("I'll look up …"). 계약 테스트가 이 설명을 최종 답으로 읽은 것을 고쳤다. 에이전트 루프는 원래 이 설명을 답으로 취급하지 않았고, 이제 이 설명도 대화 기록에 순서대로 남긴다.
  - 컨테이너 롤아웃이 이미지는 바꿨지만 aiopsd의 환경변수는 이전 것(모델 없음)을 유지해 replay로 실행됐다.
    - 이 문제는 배포 확인 단계의 provider 검사가 잡았다("provider is replay, expected meta:muse-spark-1.3").
    - 수정: 배포 직후 `POST /admin/restart-aiopsd`로 aiopsd를 재시작한다. 실행 스크립트는 `/api/v1/info`가 기대한 provider를 보일 때까지 기다린 뒤 run을 만든다.
- **결과:** run `01a19d86-d9d6-76d3-bced-54ae6d3475ec`([Deploy edge run 41500223444](https://github.com/qlanfr/mata_Hackathon/actions/runs/41500223444) 성공).

| 단계 | 내용 |
| --- | --- |
| 관측 역할 (3턴) | 1턴에 지표·요청 증거·소스 목록·과거 실험을 병렬로 조회, 2턴에 소스 4개를 읽음, 3턴에 근거 ID가 붙은 관측(p95 약 16.4 ms, 요청당 SQL 81.8개)을 작성 |
| 실험 역할 (6턴) | 가설 "N+1 in load_order_details dominates p95; fetching all items of the page plus product names in one JOIN removes most queries." 패치는 `src/repo.rs` 하나 |
| 하네스 | 정책 통과, 빌드 4.1초, golden 12/12, 불변식 208/208, 표본 비교 200/200 |
| 측정 | A/A 노이즈 2.6%, 요청당 SQL 81.8 → 3.0, p95 쌍별 개선 88.6 / 87.7 / 88.3% → **VERIFIED** |
| 사용량 | 모델 9턴, 도구 호출 15회, 후보 1개, 입력 37,327 / 출력 10,483 토큰 |

- **두 번째 Meta run** `01a19da7-b2a2-70e0-af44-018eddd0c003`(14개 golden 세트로 동기화한 배포, [Deploy edge run 41505163358](https://github.com/qlanfr/mata_Hackathon/actions/runs/41505163358) 성공):
  - 후보 1 **POLICY_REJECTED**: "the search text for `src/repo.rs` matched 0 times". 모델이 쓴 search 문자열이 원본과 정확히 일치하지 않아, 실행 전에 막혔다. 직접 쓴 함정이 아니라 **실제 모델의 실수를 하네스가 잡은 사례**다.
  - 후보 2 **VERIFIED**: 모델이 `read_trial_result`로 거절 이유를 읽고 같은 아이디어(JOIN 한 번)를 정확한 search 문자열로 다시 냈다. golden 14/14, 불변식 209/209, 표본 비교 200/200, 요청당 SQL 81.8 → 3.0, p95 쌍별 개선 88.9 / 88.7 / 89.0%.
  - 모델 14턴, 도구 호출 19회, 후보 2개, 입력 56,859 / 출력 15,877 토큰.
  - 배포 직후 첫 run은 "scenario changed"로 멈췄다. aiopsd 재시작이 이미지 롤아웃보다 먼저 일어나 aiopsd는 옛 시나리오로, runner는 새 시나리오로 떴기 때문이다. 해시 검사와 재시도가 의도대로 동작했다.
- **한계:** 시나리오 하나의 결과다. 실행 횟수와 모델별 일관성은 아래 "모델 비교" 참고.

### 변이 테스트 (11/3) — 36개 중 30개 검출, 남은 6개는 동치 4 + 우연히 맞는 2

`scripts/mutation-test.py`가 `src/repo.rs`와 `src/model.rs`에서 정해진 규칙으로 단일 변이 36개를 만든다. 규칙은 SQL 비교·정렬, bind ±1, 루프 건너뛰기·뒤집기, 산술, 개수·합계, 문장 삭제, 할당이다. 각 변이에 correctness 검사만 돌린다(`aiops-runner check`, 부하 없음).

| | 결과 |
| --- | --- |
| 첫 실행 | golden이 user 조회의 빈틈 2개를 놓쳤다. `id >= $1`이면 실제 최대 id보다 작은 없는 id(0)가 404 대신 빈 200을 받는다. `user_id + 1`을 조회하면 마지막 사용자(2010)가 404를 받는다. 각각 golden 케이스(`unknown-user-low-id`, `last-user`)를 추가했다. 기존 정답 12개는 그대로다. |
| 케이스 추가 후 | 36개 모두 컴파일, **30개 검출(83%)**. golden만으로도 30개, 불변식·표본 비교를 더해도 30개 |
| 살아남은 6개 | 동치 변이 4개: 정렬 동점 처리 2개(같은 사용자의 주문 중 `created_at`이 겹치는 경우가 seed에 0건), `with_capacity` → `new` 2개. 우연히 맞는 변이 2개: 항목 쿼리의 `ORDER BY id` 제거, 상품 조회 `id >= $1`. 둘 다 PostgreSQL이 새로 적재한 테이블을 삽입 순서대로 돌려줘서 결과가 같다. 순서가 보장되지 않는 잠재 버그지만, 이 데이터에서는 응답으로 구분할 수 없다(코드 리뷰의 영역). |
| 동치 4개를 빼면 | 32개 중 30개(94%) |

불변식과 표본 비교는 이 36개에서는 golden이 놓친 변이를 추가로 잡지 못했다. 대신 함정 모음에서 golden이 놓치는 두 경우를 잡는다. 상품 1000 크래시는 표본 비교가, 원래 코드의 규칙 위반은 불변식이 잡는다.

검사 시간 문제도 고쳤다. 모든 항목을 스캔하게 만드는 변이(`order_id >= $1`)가 표본 요청 212개를 각 10초 타임아웃까지 붙잡아, 한 변이에 26분 넘게 걸렸고 Ctrl-C도 듣지 않았다. 이제 서비스당 60초 예산 안에서만 요청하고 취소를 확인한다. 같은 변이가 133초에 검출된다.

### 모델 비교 (11/3) — Spark 1.3 7회, Wonder 1.0 4회, 같은 시나리오

실행 단위로 모델을 고를 수 있게 하고(`POST /api/v1/runs`의 `model`), **Run on Cloudflare** 워크플로로 배포본에서 순서대로 돌렸다. 배포 직후 기본 모델 run까지 포함해 Cloudflare에서 Meta 모델로 돌린 run은 아래 11개다("scenario changed"로 모델 호출 전에 멈춘 run 1개 제외).

| run | 모델 | 결과 | 후보 | p95 개선(중앙값) | 턴 / 도구 | 토큰 입력 / 출력 | 소요 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `01a19d86` | spark-1.3 | VERIFIED | 1 | 88.3% | 9 / 15 | 37.3K / 10.5K | 409초 |
| `01a19da7` | spark-1.3 | POLICY_REJECTED → VERIFIED | 2 | 88.9% | 14 / 19 | 56.9K / 15.9K | 374초 |
| `01a19f1f` | spark-1.3 | VERIFIED | 1 | 87.7% | 8 / 13 | 30.5K / 6.1K | 417초 |
| `01a19f2b` | spark-1.3 | VERIFIED | 1 | 88.0% | 8 / 13 | 33.8K / 8.8K | 381초 |
| `01a19f42` | spark-1.3 | INCONCLUSIVE 2회(편차 19.2%, 같은 패치 재측정 포함) → 2번째 후보 VERIFIED | 2 | 90.0% | 12 / 20 | 53.3K / 13.0K | 820초 |
| `01a19f63` | spark-1.3 | VERIFIED (배포 확인 run) | 1 | 88.4% | 8 / 13 | 34.8K / 9.0K | 467초 |
| `01a1a01e` | spark-1.3 | VERIFIED (화면 개편 배포 확인 run) | 1 | 89.1% | 7 / 14 | 31.1K / 15.4K | 533초 |
| `01a19f26` | wonder-1.0 | VERIFIED | 1 | 88.9% | 7 / 14 | 31.5K / 11.4K | 378초 |
| `01a19f31` | wonder-1.0 | **FAILED** (출력 상한 4,096에 잘림, 아래) | 0 | – | 8 / 15 | 35.7K / 12.4K | 231초 |
| `01a19f4f` | wonder-1.0 | VERIFIED | 1 | 90.1% | 7 / 15 | 33.5K / 10.8K | 531초 |
| `01a19f57` | wonder-1.0 | **EXHAUSTED** (계획을 쓰고 `stop`을 고름, 아래) | 0 | – | 7 / 16 | 37.0K / 15.0K | 264초 |

관찰:
- **원인 진단은 11회 모두 같았다.** 모든 가설이 `load_order_details`의 N+1을 지목했고, 제안한 수정도 모두 "페이지의 항목과 상품명을 JOIN 한 번(`ANY($1)`)으로 읽고 Rust에서 주문별로 묶기"였다. 패치 본문은 run마다 달라서(해시가 모두 다름) 검증은 매번 실제로 필요했다. VERIFIED 패치의 개선율은 87.7~90.1%다.
- **Spark 1.3은 7회 모두 VERIFIED로 끝났다.** 2회는 두 번째 후보가 필요했다. 한 번은 첫 패치의 search 문자열이 원본과 달라 실행 전에 막혔고, 한 번은 첫 패치가 두 번 측정 모두 baseline 편차로 보류됐다(19.2%).
- **Wonder 1.0은 4회 중 2회 VERIFIED다.** 실패 1회는 우리 쪽 상한 때문이었다. 요청에 `max_output_tokens: 4096`을 걸어 두었는데 Wonder는 답하기 전에 생각(reasoning) 토큰을 많이 써서, 마지막 답변 두 번이 정확히 4,096에서 잘렸다(한 번은 빈 답, 한 번은 118자에서 끊김). 상한을 16,384로 올리고, API가 `status: incomplete`를 돌려주면 답을 파싱하지 않고 이유를 기록한 뒤 보정 턴에서 짧게 다시 답하게 했다. 다른 run에서도 4,096에 닿은 턴이 4번 있었는데 운 좋게 JSON이 끝난 뒤였다.
- 나머지 1회는 모델이 가설·패치까지 다 쓰고 `action`을 `stop`으로 골랐다(stop_reason: "ready to proceed under local verification"). 프로토콜 오해다. 지금은 패치가 들어 있는 `stop` 답변을 거절하고 보정 턴에서 `experiment`로 다시 내게 했다. 이 수정은 아직 run으로 검증하지 않았다.
- Wonder의 패치 설명은 더 꼼꼼했다(입력 순서 유지, 빈 페이지 처리, 상품 누락 시 오류 의미 유지를 명시). 입력 토큰은 비슷하고 출력 토큰은 Wonder가 더 많다. Wonder 4회의 모델 비용은 화면 요금표(입력 $4, 출력 $20 / 100만 토큰)로 약 $1.5다. Spark 요금은 확인하지 못했다.
- **결론: 기본 모델은 `muse-spark-1.3`을 유지한다.** 이 시나리오에서 Wonder의 이점이 보이지 않았고, Spark가 더 안정적이고 더 쌌다.

이번 비교에서 함께 확인된 것:
- **R2:** inline 한도를 4 KB로 내려 모든 보고서가 R2를 거치게 했다. run `01a19f42`의 아티팩트 4개가 R2에 저장됐고, 실행 스크립트가 API로 다시 읽어 왔다. 처음에는 적용되지 않았는데, Cloudflare가 2026-10-30부터 Durable Object 코드 갱신을 기본 5분 지연시켜 재시작된 컨테이너가 옛 환경변수를 받았기 때문이다. DO 갱신을 `immediate`로 바꾸고, 배포마다 스탬프를 넣어 aiopsd가 그 스탬프를 보고할 때까지 재시작하게 했다.
- **cron:** Worker의 1분 cron이 aiopsd에 실제로 도착한다. 418초 run에 7회, 820초 run에 14회, 즉 1분에 1회다.

### 실제 버그 확인 (11/3) — Vaultwarden #1402 N+1이 runner와 같은 조건에서 재현됨

- 배경: 시나리오가 직접 만든 N+1 하나라는 약점. 공개 데이터셋을 조사했다(SWE-Perf, GSO, SWE-fficiency, Rails 성능 버그 연구, AIOps 장애 데이터셋 등). 대부분 Python 함수 실행 시간 벤치마크이거나, 코드 수정 없이 로그·지표만 있어 우리 하네스(HTTP + PostgreSQL + p95)에 바로 맞지 않았다.
- 가장 잘 맞는 실제 버그: Vaultwarden(Rust Bitwarden 서버) issue #1402 "N+1 query problem on api/sync", 수정 PR #2429(2022-05-11 병합, 커밋 `3ca85028`).
- 확인 방법: **Real bug probe** 워크플로가 수정 전 커밋과 수정 커밋을 Rust 1.60으로 빌드하고, 같은 금고(폴더 20, 항목 N개)를 API로 넣은 뒤 `/api/sync`를 10번 호출해 `pg_stat_statements`로 SQL 수를 셌다. DB는 같은 머신.
- 결과([run 41592017741](https://github.com/qlanfr/mata_Hackathon/actions/runs/41592017741)):

| 버전 | 항목 | sync당 SQL | sync 중앙값 |
| --- | --- | --- | --- |
| 수정 전 | 100 | 411 | 28.1 ms |
| 수정 전 | 1000 | 4,011 | 249.6 ms |
| 수정 후 | 100 | 17 | 6.8 ms |
| 수정 후 | 1000 | 17 | 44.0 ms |

- 항목 하나마다 쿼리 4개(첨부, 즐겨찾기, 폴더, 컬렉션)가 나간다. 수정본은 항목 수와 상관없이 17개다.
- 처음 두 번은 빌드 단계에서 실패했다. Debian 11 저장소가 archive로 옮겨졌고, 저장소의 `rust-toolchain`이 "stable"이라 최신 컴파일러가 쓰였다. 기본 이미지의 libpq를 쓰고 `RUSTUP_TOOLCHAIN=1.60.0`으로 고정해 해결했다.
- 아직 모델 루프는 돌리지 않았다. 필요한 하네스 변경은 `docs/real-bug-probe.md`에 적었다.

## 검증 방법

- `cargo test -p aiops-core`: 상태 전이 표, 예산 초과 거절, policy(범위 밖 경로·symlink) 거절, Evaluator 규칙(중앙값·방향 일치·게이트) 단위 테스트.
- `cargo test -p aiopsd`: in-memory SQLite에 `migrations/*.sql`을 적용하고 CAS 경합(같은 `state_version`으로 두 번 갱신 → 한 번만 성공), batch 원자성, `run_events.seq` 연속성, 중복 알림 병합을 확인한다.
- `docker compose -f deploy/docker-compose.yml up` 후 스크립트로 정상 후보 → VERIFIED, 필드 누락 후보 → REJECTED, k6 rate 과다(dropped) → INCONCLUSIVE를 재현한다.
- ReplayProvider로 `POST /runs` E2E: run_events 순서·상태 최종값·artifact 해시를 확인한다.
- 취소: 실행 중 `POST /cancel` → runner 프로세스 그룹 종료 확인(`ps`) → CANCELLED.
- 재시작: aiopsd를 재기동한 뒤 tick이 이어서 진행하는지, runner를 재기동하면 boot_id 불일치가 감지되는지 확인한다.
- CF 배포본: `wrangler d1 migrations apply --remote` 후 같은 E2E를 1회 이상 수행하고, run ID·baseline SHA·원본 측정을 README에 링크한다. runner 컨테이너에서 `d1.internal` 호출이 실패하는지(접근 불가)도 확인한다.
