# AIOps 자율 실험 엔지니어 — 해커톤 구현 계획 (Rust + React + Cloudflare)

## Context

- 목표: 업로드한 기획안 v0.1을 바탕으로 Meta Global Hackathon(마감 **2026-11-04 21:00 ET = 한국 11/5 11:00**)에 제출할 MVP를 만든다.
- 제품 정의(기획안 그대로): Meta 모델이 운영 증거를 읽고 가설과 패치를 만든다. Rust 하네스는 격리 환경에서 패치를 빌드·검증·측정한다. 판정(VERIFIED / REJECTED / INCONCLUSIVE)은 Rust 평가기만 내린다.
- 첫 시나리오: Rust 주문 목록 API의 **N+1 조회**. 목표는 정확성을 유지하면서 p95 지연을 20% 이상 개선하는 것이다.
- 기획안과 달라진 점(사용자 결정):
  - UI: Askama SSR 대신 **React SPA**.
  - 배포: Docker Compose 대신 **Cloudflare Containers 중심**.
  - 인원: **1인 + Claude**.
  - Meta API 키: **아직 없음**. 개발 초기에는 replay provider를 쓴다.
- 저장소: `qlanfr/mata_hackathon`이고 현재 비어 있다(커밋 0개). 브랜치는 `claude/busy-cannon-5b5n40`. 기획안이 가리키는 `qlanfr/aiops`는 참고만 한다.

### 사실 / 가정 구분

| 구분 | 내용 |
| --- | --- |
| 확인한 사실 (CF 공식 문서) | Containers와 Sandboxes는 2026-04-13 GA. 인스턴스는 최대 4 vCPU / 12 GiB / 20 GB(`standard-4`). Container 안에서 도는 코드는 activity로 치지 않는다. 그래서 요청·alarm·`renewActivityTimeout()`이 없으면 `sleepAfter`(기본 10분) 뒤에 정지된다. `enableInternet`은 Container class 기본값이 true다. `outboundByHost`로 컨테이너 → Worker 바인딩(다른 Container DO 포함) 호출이 가능하다. 플랫폼 종료 시 SIGTERM을 보내고 최대 15분 기다린다. |
| 확인한 사실 (이 환경) | cargo 1.97, node 22, pnpm, psql 16, docker CLI가 있다. k6와 wrangler는 없다(npx로 사용). |
| 미확인 (네트워크 차단) | cerebralvalley.ai와 dev.meta.ai 페이지는 열리지 않았다. Meta API 형식(Responses API, `tool_choice=auto`만 지원, `text.format` 구조화 출력)은 **기획안 서술을 그대로 따른 것**이다. 키를 받으면 첫 작업으로 검증한다. |
| 가정 | Workers Paid 플랜($5/월)을 쓸 수 있다. 제어 DB로 Neon(Postgres) 무료 계정을 만든다. 배포(`wrangler deploy` 이미지 빌드)는 사용자 로컬 Docker나 GitHub Actions에서 실행한다. |

## 아키텍처

```
Browser (React SPA)
  │  GET 공개 / POST는 Bearer ADMIN_TOKEN
  ▼
Cloudflare Worker  (edge/, TypeScript ~150줄, 라우팅·인증·컨테이너 수명만 담당 — 제품 로직 없음)
  ├─ Static Assets: web/dist
  ├─ Cron Trigger (1분) ──► aiopsd /internal/tick  (오케스트레이터 keep-alive + 재개)
  ├─ /api/* ──► AiopsdContainer (DO, standard-1, enableInternet=true)
  │              Rust Axum: API · SSE · 상태머신 · 에이전트 하네스 · 평가기
  │              ├─ Neon Postgres (runs/trials/jobs/run_events/artifacts/experiment_cases)
  │              └─ Meta API (api.meta.ai/v1)  ← 키 없으면 ReplayProvider
  │              │ http://runner.internal/*  (outboundByHost → env.RUNNER.getByName("runner-0"))
  ▼
RunnerContainer (DO, standard-4, enableInternet=false)
   Rust aiops-runner: 작업공간 생성 · 패치 적용 · cargo build --offline
   · 내장 Postgres(seed 미리 적재) · 정확성 검사 · k6 부하 · 프로세스 그룹 정리
```

### 핵심 설계 선택과 이유

1. **runner는 인터넷을 차단하고 DB 자격증명도 주지 않는다.** 의존성은 이미지에 vendoring해 `CARGO_NET_OFFLINE=true`로 빌드한다. 결과는 aiopsd가 polling으로 가져가 저장한다. 그래서 runner에는 비밀값이 하나도 없다. 기획안 7.3의 격리 요구를 CF 기본 기능으로 만족한다.
2. **컨테이너 간 통신은 outbound handler를 거친다.** aiopsd가 `http://runner.internal/jobs`를 호출하면 Worker의 `outboundByHost`가 받아 Runner DO stub의 fetch로 넘긴다. aiopsd는 작업 중에 `GET /jobs/{id}`를 5초마다 polling한다. 이 polling이 상태 확인이면서 runner keep-alive 역할도 한다.
3. **keep-alive와 재개는 Cron이 맡는다.** 브라우저가 닫히면 aiopsd에 요청이 끊긴다. 그래서 Worker Cron이 1분마다 `/internal/tick`을 호출한다. tick은 DB에서 종료되지 않은 run을 찾아 한 단계씩 진행한다. 상태 전이는 `state_version` CAS를 쓰는 짧은 트랜잭션이다. 이렇게 하면 재시작해도 이어서 진행된다(기획안 6.2).
4. **runner 재시작은 boot_id로 감지한다.** runner는 응답마다 `boot_id`를 실어 보낸다. 바뀌면 해당 attempt를 `RUNNER_LOST`로 기록하고, 아티팩트를 대조한 뒤에만 재시도한다. 같은 작업을 확인 없이 다시 실행하지 않는다.
5. **패치는 search/replace 블록으로 받는다.** 모델이 unified diff를 정확히 만들어 내는 경우가 드물다. 그래서 `{path, search, replace}[]` 형식으로 받는다. runner는 search 문자열이 정확히 1회 일치하는지 확인한 뒤 적용한다. 화면에 보여 줄 diff는 runner가 `git diff`로 생성한다. 허용 경로는 `fixtures/orders-api/src/**`이고 `tests`, `Cargo.*`는 제외한다. 경로 정규화와 symlink 검사를 거친다.
6. **provider는 trait 하나로 추상화한다.** `LlmProvider` 구현은 두 가지다. `MetaProvider`는 reqwest로 Responses API를 호출한다. `ReplayProvider`는 `prompts/replay/n-plus-one/*.json`의 스크립트 응답을 쓴다. UI에 provider 배지를 표시해 replay 실행을 실제 모델 실행처럼 보이지 않게 한다.
7. **trial 실행은 상태머신이 주도한다.** 모델은 계획과 패치만 제안한다. `request_trial` 도구는 쓰지 않고, 오케스트레이터가 정책·예산을 검사한 뒤 job을 만든다. 1인 개발에 맞춰 결정성과 디버깅 용이성을 우선한 선택이다.

## 저장소 구조 (신규)

```
Cargo.toml                 # workspace: crates/*, apps/*  (fixtures는 exclude)
crates/aiops-core/         # domain 타입, RunState 상태머신, Policy, Budget, ScenarioSpec,
                           # LlmProvider trait + Meta/Replay, 에이전트 루프, Evaluator(판정 규칙)
apps/aiopsd/               # Axum API, SSE, SQLx(Postgres), orchestrator tick, runner client
apps/aiops-runner/         # Axum job API, workspace/patch, build·check·bench, cleanup(pgid kill)
fixtures/orders-api/       # 개선 대상: Axum+SQLx(런타임 쿼리, 매크로 없음), N+1 포함, 요청별 JSON 로그
scenarios/n-plus-one/      # scenario.toml(허용경로·예산·계약), seed.sql, k6.js, cases/*.json(골든)
migrations/                # 제어 DB 스키마
prompts/                   # observer.md, experimenter.md, schemas/*.json, replay/
web/                       # React + Vite + TS, TanStack Query, react-router, EventSource
edge/                      # wrangler.jsonc, src/index.ts (Worker + 2 Container 클래스 + cron)
deploy/                    # Dockerfile.aiopsd, Dockerfile.runner, docker-compose.yml(로컬)
docs/                      # architecture.md(mermaid), decisions.md, demo-script.md
```

## 데이터·API (기획안 10장 축소판)

- 테이블: `runs`(fingerprint, target_sha, scenario_hash, state, state_version, budget, cancel_requested, provider), `trials`(hypothesis, plan, patch_hash, verdict, reason), `jobs`(kind, lease, runner_boot_id, attempt), `run_events`(seq, kind, payload — 모델 사용량·도구 호출·상태 변경), `artifacts`(kind, sha256, size, bytea/jsonb — MVP는 DB에 저장, R2는 P1), `experiment_cases`(조건, 결과, 검증 수준).
- `incidents` 테이블은 별도로 만들지 않는다. `runs.fingerprint`에 활성 상태 부분 unique index를 걸어 중복 알림을 병합한다.
- API는 기획안의 7개 그대로다: `POST /alerts`, `POST /runs`, `GET /runs/{id}`, `GET /runs/{id}/events`(SSE + `?after_seq=` polling 겸용), `POST /runs/{id}/cancel`, `GET /trials/{id}`, `GET /artifacts/{id}`. 목록용 `GET /runs`를 하나 추가한다.

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
| **D1 · 11/2** | workspace 골격; fixture API(N+1) + seed + 골든 케이스 + k6; aiops-runner 로컬 실행(내장 PG, build, check, bench, cleanup); Evaluator; 수동 후보 2개(정상 JOIN 버전, 필드 누락 버전). **CF 탐색: runner 이미지를 Container에 올려 cargo 증분 빌드 시간·A/A 변동 실측** | 로컬에서 정상 후보 → VERIFIED, 누락 후보 → REJECTED. CF에서 runner 1회 왕복 |
| **D2 · 11/3** | migrations + aiopsd 상태머신/tick/jobs/lease; runner client(outbound 경유); run_events + SSE; LlmProvider(Replay 먼저, 키가 오면 Meta); 관측·실험 두 역할 + 도구(read_request_evidence, read_source, search_experiments, submit_patch, read_trial_result); 예산·취소; edge Worker + cron 배포 | CF 배포본에서 `POST /runs` → replay(또는 Meta) 제안 → runner 검증 → 판정까지 완주 |
| **D3 · 11/4** | React 3화면(실행 목록 / 상세 타임라인 / 결과 비교·diff); 복구·취소·중복 알림 검증; experiment_cases 기록; README·재현 가이드·데모 녹화 | 다른 사람이 README대로 재현 가능, 데모 영상 완성 |
| 11/5 오전 | 버퍼 + 제출 | 11:00 전 제출 |

D2가 끝날 때 완주하지 못하면 다음 순서로 범위를 줄인다: `POST /alerts` → experiment_cases 검색 → UI 장식 → 독립 확인 조건.

## 1인 개발을 위한 범위 축소 (기획안 P0 대비)

- Prometheus/Grafana는 P1로 미룬다. 관측 증거는 fixture가 남기는 요청별 JSON 로그(request_id, sql_count, db_ms, latency_ms, status)와 k6 summary로 대체한다.
- pgvector, OpenTelemetry, GitHub PR 생성, R2 아티팩트는 P1로 미룬다.
- 최종 독립 확인(별도 seed 1쌍)은 시간이 남을 때만 한다.

## 리스크와 반례

| 리스크 | 대응 |
| --- | --- |
| CF Container 측정 잡음이 커서 20% 판정이 흔들림 | N+1 → JOIN은 보통 수 배 차이가 나서 여유가 크다(추정이며 D1에 실측). A/A 게이트로 INCONCLUSIVE 처리. 그래도 불안정하면 부하·데이터 크기를 키운다 |
| standard-4에서 cargo 증분 빌드가 너무 느림 | 이미지에 baseline release 빌드와 `target/`을 미리 굽는다. 그래도 느리면 `opt-level=1` 프로필을 시나리오 계약에 고정한다 |
| Worker → Container SSE 스트리밍 문제 | events API를 `after_seq` polling과 같은 커서로 설계해 두고, 문제가 생기면 프론트를 2초 polling으로 전환한다 |
| Meta API 키가 늦게 옴 / 형식이 기획안과 다름 | provider trait 뒤에 숨겨 둔다. 키를 받은 날 계약 테스트(JSON 출력, 도구 왕복, 사용량)를 먼저 돌린다. 끝까지 키가 없으면 replay로 데모하되 UI와 영상에 "REPLAY"를 명시한다 |
| 로컬 Docker 없이 배포 불가 | GitHub Actions에서 `wrangler deploy`를 실행하는 워크플로를 둔다(CF API 토큰은 사용자가 secret으로 등록) |
| 모델 패치가 search 불일치나 범위 밖 경로를 가리킴 | policy 거절을 run_event로 남기고, 구조화 응답 보정 1회 후 다음 턴으로 넘긴다(예산 차감) |

## 다음 산출물 (계획 승인 후)

1. **시스템 흐름도**: `/archify`는 이 세션에 스킬로 설치돼 있지 않고 플러그인 카탈로그에서도 찾지 못했다. 그래서 같은 목적의 다이어그램을 직접 만든다.
   - (a) 컴포넌트 구성도와 실험 루프 시퀀스(상태 전이 포함)를 담은 HTML Artifact.
   - (b) 같은 내용을 `docs/architecture.md`에 mermaid로 저장해 저장소에 커밋.
2. 이 계획을 `docs/plan.md`로 저장하고 커밋한 뒤 `claude/busy-cannon-5b5n40`에 push한다.
3. 이어서 D1 구현을 시작한다(사용자가 원할 때).

## 검증 방법

- `cargo test -p aiops-core`: 상태 전이 표, 예산 초과 거절, policy(범위 밖 경로·symlink) 거절, Evaluator 규칙(중앙값·방향 일치·게이트) 단위 테스트.
- `docker compose -f deploy/docker-compose.yml up` 후 스크립트로 정상 후보 → VERIFIED, 필드 누락 후보 → REJECTED, k6 rate 과다(dropped) → INCONCLUSIVE를 재현한다.
- ReplayProvider로 `POST /runs` E2E: run_events 순서·상태 최종값·artifact 해시를 확인한다.
- 취소: 실행 중 `POST /cancel` → runner 프로세스 그룹 종료 확인(`ps`) → CANCELLED.
- 재시작: aiopsd를 재기동한 뒤 tick이 이어서 진행하는지, runner를 재기동하면 boot_id 불일치가 감지되는지 확인한다.
- CF 배포본에서 같은 E2E를 1회 이상 수행하고, run ID·baseline SHA·원본 측정을 README에 링크한다.
