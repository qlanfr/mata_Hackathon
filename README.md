# AIOps 자율 실험 엔지니어

Meta 모델이 운영 증거를 읽고 성능 개선 가설과 패치를 제안합니다. Rust 하네스는 그 패치를 격리된 Cloudflare Container에서 빌드·검증·측정합니다. 채택 여부는 Rust 평가기만 판정합니다.

- 첫 시나리오: Rust 주문 목록 API + PostgreSQL의 N+1 조회
- 스택: Rust(Axum · Tokio) / React(Vite · TypeScript) / Cloudflare Workers + Containers + D1 + R2
- 상태: 1일차. 실험 하네스(runner + 평가기)는 로컬에서 동작을 확인했습니다. 오케스트레이터, 모델 연결, 화면은 아직 없습니다.

## 지금 동작하는 것

| 구성 요소 | 위치 | 역할 |
| --- | --- | --- |
| `aiops-core` | `crates/aiops-core` | 시나리오 계약, search/replace 패치 정책, 측정 보고서 타입, 판정 게이트(평가기) |
| `aiops-runner` | `apps/aiops-runner` | 패치 적용 → 빌드 → golden 검사 → k6 교대 측정 → 정리. CLI와 HTTP job API |
| 실험 대상 | `fixtures/orders-api` | N+1 패턴이 있는 주문 목록 API. 요청마다 SQL 수와 DB 시간을 JSON으로 기록 |
| 시나리오 | `scenarios/n-plus-one` | seed, golden 케이스 11개, 부하 스크립트, 하네스 검증용 후보 3개 |
| Edge Worker | `edge` | runner 컨테이너와 관리자 토큰 경로(1일차 실측용) |

로컬 측정 결과(4 vCPU 개발 컨테이너, 100 rps)는 [docs/plan.md의 진행 상황](docs/plan.md#진행-상황)에 있습니다.
정상 후보는 VERIFIED(p95 약 48 ms → 약 4.5 ms), 항목을 빠뜨린 후보는 REJECTED, 보호 파일을 고친 후보는 POLICY_REJECTED, 과부하 측정은 INCONCLUSIVE로 판정됐습니다.

## 실행

```bash
cargo test --workspace
```

```bash
cargo build --release -p aiops-runner
```

```bash
./target/release/aiops-runner --pg-user postgres trial --patch scenarios/n-plus-one/candidates/known-good-batched-join.json
```

PostgreSQL 서버 바이너리와 k6가 필요합니다. 배포와 준비 작업은 [docs/setup.md](docs/setup.md)를 보세요.

## 문서

- [docs/plan.md](docs/plan.md): 구현 계획, 일정, 리스크, 진행 상황
- [docs/architecture.md](docs/architecture.md): 배포 구성, 상태 전이, trial 시퀀스, 판정 게이트(mermaid)
- [docs/setup.md](docs/setup.md): 사용자가 할 일, Cloudflare 배포 방법, 실측 스크립트
