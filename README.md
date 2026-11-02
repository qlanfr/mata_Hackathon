# AIOps 자율 실험 엔지니어

Meta 모델이 운영 증거를 읽고 성능 개선 가설과 패치를 제안합니다. Rust 하네스는 격리된 Cloudflare Container에서 그 패치를 빌드·검증·측정합니다. 채택 여부는 Rust 평가기만 판정합니다.

- 첫 시나리오: Rust 주문 목록 API + PostgreSQL의 N+1 조회
- 스택: Rust(Axum · Tokio · SQLx) / React(Vite · TypeScript) / Cloudflare Workers + Containers / Neon Postgres
- 상태: **구현 전 설계 단계.** 아직 실행 가능한 코드나 측정 결과가 없습니다.

## 문서

- [docs/plan.md](docs/plan.md): 해커톤 구현 계획(범위, 일정, 리스크, 검증 방법)
- [docs/architecture.md](docs/architecture.md): 배포 구성, 상태 전이, trial 시퀀스, 판정 게이트(mermaid)
