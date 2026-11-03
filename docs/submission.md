# 제출 체크리스트

마감: 2026-11-04 21:00 ET (한국 2026-11-05 11:00). 기획안 기준이며, 최종 규정은 참가자 페이지에서 다시 확인합니다.

## 사용자가 해야 할 일

- [ ] 참가 등록과 팀 정보 확인
- [ ] 제출 규정 확인: 기존 코드 사용 허용 여부, 필수 항목(저장소, 영상, 데모 URL, 설명문 길이 등)
- [x] Cloudflare 배포 (`docs/setup.md`)와 배포 URL에서 run 1회 완주
- [x] Meta API 키를 받으면 `META_API_KEY` secret과 `META_MODEL` 설정 후 Meta provider로 run 1회 (`muse-spark-1.3`, run `01a19d86-d9d6-76d3-bced-54ae6d3475ec`, VERIFIED)
- [ ] 데모 영상 녹화 (`docs/demo-script.md`), 빨리 감기 구간과 run ID 표시
- [ ] 제출 폼 작성과 업로드, 11:00 KST 전에 여유 있게

## 제출 설명에 쓸 수 있는 사실 (측정됨)

### Meta 모델로 돌린 배포본 run (가장 중요)

- 모델 `muse-spark-1.3`(Meta Model API)이 관측·실험 역할을 맡았다. run `01a19d86-d9d6-76d3-bced-54ae6d3475ec`.
- 모델이 도구로 지표·요청 증거·소스를 읽었다. "load_order_details의 N+1이 p95를 지배한다"는 가설을 세우고, 한 페이지의 항목과 상품명을 JOIN 한 번으로 읽는 패치를 직접 작성했다.
- 하네스 검증: golden 12/12, 불변식 208/208, 표본 비교 200/200. 요청당 SQL 81.8 → 3.0, p95 쌍별 개선 88.6 / 87.7 / 88.3%, A/A 노이즈 2.6%. 판정 VERIFIED.
- 모델 9턴, 도구 호출 15회, 입력 37,327 / 출력 10,483 토큰. 생성부터 판정까지 약 7분(배포 직후 runner 교체로 인한 재시도 포함).
- 두 번째 run(`01a19da7-b2a2-70e0-af44-018eddd0c003`): 모델의 첫 패치가 search 문자열 불일치로 POLICY_REJECTED(실행 전 차단)됐다. 모델이 거절 이유를 읽고 고친 두 번째 패치가 VERIFIED(p95 쌍별 개선 88.9 / 88.7 / 89.0%, golden 14/14)를 받았다. 실제 모델의 실수를 하네스가 막고, 루프가 스스로 회복한 사례다.
- 모델 비교(같은 시나리오, Cloudflare): `muse-spark-1.3` 5회 모두 VERIFIED(2회는 두 번째 후보로), `muse-wonder-1.0` 4회 중 2회 VERIFIED. 9회 모두 같은 원인(N+1)과 같은 종류의 수정(JOIN 한 번)을 냈고, 패치 본문은 매번 달랐다. 개선율 87.7~90.1%. 표는 `docs/plan.md`.
- 표현 주의: 시나리오 하나에서 Spark 5회, Wonder 4회를 돌린 결과다. "모델이 항상 찾는다"고 쓰지 않는다. Wonder의 실패 1회는 우리 쪽 출력 상한 때문이었다고 밝힌다.

### Cloudflare 배포본 (https://aiops-edge.yh9365.workers.dev, Replay provider)

Cloudflare Containers, runner 4 vCPU, PostgreSQL 15.19, k6 v0.57.0, 100 rps, 20초 × 3쌍. 숫자는 run `01a19cf1-3ac6-7003-a2cb-1c4fa1c179d4` 기준입니다.

- baseline: 요청당 SQL 평균 81.9개(최대 182), p95 약 17.6~17.9 ms.
- 정상 후보(JOIN 1회): p95 약 2.0 ms, 쌍별 개선 89.1 / 88.7 / 88.6%, 요청당 SQL 3.0개, golden 11/11 → VERIFIED.
- A/A 노이즈 1.3%, 후보 빌드 4.5초(인터넷 차단 컨테이너, 오프라인 빌드). run 생성부터 판정까지 약 6분.
- 같은 날 다른 run 두 개도 VERIFIED였다(쌍별 개선 88.7~90.2%). baseline p95는 인스턴스에 따라 17~29 ms로 달랐다.
- 배포 중 aiopsd와 runner 컨테이너가 모두 교체돼도 run이 이어서 완료됐다(작업 유실 → 재시도, 리스 인계).

### 하네스 함정 모음 (로컬, 14개)

- 평가기 관문마다 직접 작성한 함정 11개와 기존 후보 3개, 모두 기대 판정대로 나왔다(효과 없는 변경은 INCONCLUSIVE). 표: `docs/plan.md`.
- 함정을 만들다가 golden의 빈틈을 찾아 메웠다. 항목 순서가 바뀐 오답이 VERIFIED를 받던 문제다. 케이스 추가 후 REJECTED.
- 표현 주의: 함정은 직접 쓴 하네스 검증용이다. "모델의 오답을 잡았다"고 쓰지 않는다.

### 변이 테스트 (로컬, 규칙 기반 36개)

- 사람이 고르지 않은 변이 36개 중 30개를 correctness 검사가 잡았다. 남은 6개 중 4개는 응답을 바꿀 수 없는 동치 변이이고, 2개는 이 데이터에서 결과가 우연히 같은 잠재 버그(정렬 없는 SQL)다.
- 첫 실행이 golden의 빈틈 2개(없는 사용자 id 0, 마지막 사용자 2010)를 찾아내 케이스를 추가했다.

### 로컬 개발 컨테이너

로컬 개발 컨테이너(4 vCPU, rustc 1.97, PostgreSQL 16, k6 v0.57.0), 100 rps, 20초 × 3쌍 기준입니다. Cloudflare에서 측정한 값이 아닙니다.

- N+1 baseline: 요청당 SQL 약 82개, p95 약 46~54 ms.
- 정상 후보(JOIN 1회): p95 약 4.7~5.6 ms, 쌍별 개선 88.9 / 91.3 / 90.7%, 요청당 SQL 3.0개, golden 11/11 → VERIFIED.
- 항목 누락 후보(하네스 검증용으로 직접 작성): golden 6/11 → REJECTED.
- `Cargo.toml` 수정 후보: 실행 전에 POLICY_REJECTED.
- 1,500 rps 과부하: baseline이 반복 6,332건 누락 → INCONCLUSIVE.
- 측정 중 다른 작업으로 CPU 간섭: baseline 편차 19.7% → INCONCLUSIVE (잘못된 VERIFIED 없음).
- `POST /api/v1/runs` 한 번으로 baseline → 관측 → 실험 → 빌드 → 검사 → 측정 → 판정까지 290초.
- 취소, 알림 병합, aiopsd 강제 종료 후 재개, runner 재시작 후 재시도 확인.

## 쓰면 안 되는 표현

- "Meta 모델이 N+1을 찾아 고쳤다" → 위 run(`01a19d86…`)을 근거로 쓸 수 있습니다. Replay run 화면을 이 문장과 함께 쓰지 않습니다.
- "Cloudflare에서 p95 90% 개선" → Replay provider로 측정한 결과입니다. 모델 쪽은 녹화된 스크립트였다고 함께 밝힙니다.
- "모든 부하에서 개선", "동치 증명" → golden 12개와 고정 부하에서의 결과일 뿐입니다.
- "Rust라서 루프가 빠르다" → 전체 시간은 빌드·측정이 대부분이고, 이를 비교 측정하지 않았습니다.
