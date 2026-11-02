# 제출 체크리스트

마감: 2026-11-04 21:00 ET (한국 2026-11-05 11:00). 기획안 기준이며, 최종 규정은 참가자 페이지에서 다시 확인합니다.

## 사용자가 해야 할 일

- [ ] 참가 등록과 팀 정보 확인
- [ ] 제출 규정 확인: 기존 코드 사용 허용 여부, 필수 항목(저장소, 영상, 데모 URL, 설명문 길이 등)
- [ ] Cloudflare 배포 (`docs/setup.md`)와 배포 URL에서 run 1회 완주
- [ ] Meta API 키를 받으면 `META_API_KEY` secret과 `META_MODEL` 설정 후 Meta provider로 run 1회 (받지 못하면 Replay로 제출하고 그 사실을 명시)
- [ ] 데모 영상 녹화 (`docs/demo-script.md`), 빨리 감기 구간과 run ID 표시
- [ ] 제출 폼 작성과 업로드, 11:00 KST 전에 여유 있게

## 제출 설명에 쓸 수 있는 사실 (측정됨)

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

- "Meta 모델이 N+1을 찾아 고쳤다" → Replay로 찍었다면 사실이 아닙니다. Meta로 실제 run을 완주한 경우에만 씁니다.
- "Cloudflare에서 p95 90% 개선" → 배포 환경에서 측정하기 전까지는 로컬 측정이라고 명시합니다.
- "모든 부하에서 개선", "동치 증명" → golden 11개와 고정 부하에서의 결과일 뿐입니다.
- "Rust라서 루프가 빠르다" → 전체 시간은 빌드·측정이 대부분이고, 이를 비교 측정하지 않았습니다.
