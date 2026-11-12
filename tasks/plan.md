# Plan: SPEC.md 구현

작업은 위에서 아래로 의존한다. 각 작업은 실패하는 시험 → 구현 → 전체 시험 → 커밋 순서로 끝낸다.
"샘플 시험"은 `cargo test -- --ignored`(`samples/` 필요), 나머지는 `cargo test`로 검증한다.

## 1단계: 기반

- [x] **T1. 의존성과 빌드 환경**
  - Acceptance: `truehd`, `ffmpeg-next`가 링크된 상태로 기존 시험 10개가 통과한다. 컨테이너 빌드에 필요한 패키지가 README에 적혀 있다.
  - Verify: `cargo test`, `cargo clippy --all-targets -- -D warnings`
  - Files: `Cargo.toml`, `Cargo.lock`, `README.md`
  - Risk: Alpine(musl)에서 `ffmpeg-next`의 bindgen 빌드가 안 되면 Debian 기반 이미지로 바꾼다.

- [x] **T2. 스피커 배치와 표준 채널 위치** (`src/layout.rs`)
  - Acceptance: 프리셋·JSON 배치에서 스피커별 방향 벡터·역할·이름을 얻는다. 표준 채널(FL, FR, FC, LFE, Ls, Rs, Lb, Rb, 천장 등)의 방위각·고도 표가 있다.
  - Verify: 단위 시험 (5.1.2 픽스처 배치, 7.1.4 프리셋)
  - Files: `src/layout.rs`, `src/main.rs`

- [x] **T3. VBAP 게인** (`src/render.rs`)
  - Acceptance: 임의 방향에 대해 스피커 게인을 낸다. 스피커 방향이면 그 스피커만 1, 두 스피커 사이면 그 둘만, 항상 제곱합 1. 높이 스피커가 없는 평면 배치에서도 동작한다.
  - Verify: 단위 시험
  - Files: `src/render.rs`

- [x] **T4. 베드 렌더** (`src/render.rs`)
  - Acceptance: 채널 이름이 붙은 PCM 블록을 출력 배치로 보낸다. 5° 이내 스피커가 있으면 그 하나로만, LFE는 `lfe` 스피커로(없으면 버림).
  - Verify: 단위 시험 (7.1 → 7.1 항등, 7.1 → 5.1에서 Lb가 Ls 쪽으로, LFE 없는 배치)
  - Files: `src/render.rs`

- [ ] **T5. 디코더 분리 리팩터** (동작 변화 없음)
  - Acceptance: OpenJOC 세션 코드가 `src/decode/joc.rs`로 옮겨지고 `main`은 버스트 타입에 따라 디코더를 고른다. 기존 E2E 출력이 바이트 단위로 같다.
  - Verify: `cargo test`
  - Files: `src/main.rs`, `src/decode/mod.rs`, `src/decode/joc.rs`

- [ ] **T6. 입력 형식 옵션**
  - Acceptance: `--input-channels 2|8`, `--input-rate`(기본 192000). 8채널 캡처에서 전체 채널에 실린 버스트와 앞 2채널에만 실린 버스트를 모두 찾는다. 무음 채움이 입력 형식에 맞게 계산된다.
  - Verify: 단위 시험 + E2E (기존 JOC 픽스처를 8채널로 펼친 입력이 2채널 입력과 같은 출력)
  - Files: `src/main.rs`, `src/iec61937.rs`, `tests/e2e.rs`

## 2단계: FFmpeg 경로

- [ ] **T7. AC-3와 JOC 없는 E-AC-3** (`src/decode/ffmpeg.rs`)
  - Acceptance: 채널마다 다른 주파수의 사인파로 만든 AC-3 5.1, E-AC-3 5.1 픽스처가 같은 배치에서 맞는 채널로만 나온다. 48 kHz가 아니면 알리고 무음.
  - Verify: E2E + 샘플 시험(성공 기준 1의 AC-3, E-AC-3 5.1/7.1)
  - Files: `src/decode/ffmpeg.rs`, `src/main.rs`, `tests/e2e.rs`, `tests/samples.rs`, `tests/fixtures/*`

- [ ] **T8. OpenJOC 거부 시 대체 디코딩**
  - Acceptance: OpenJOC가 스트림을 거부하면 FFmpeg 코어 디코딩으로 넘어가고 그 사실을 한 번 알린다. 정상 JOC 스트림의 출력은 바뀌지 않는다.
  - Verify: 샘플 시험(성공 기준 2, 5)
  - Files: `src/main.rs`, `src/decode/joc.rs`, `tests/samples.rs`

- [ ] **T9. DTS 계열**
  - Acceptance: DTS(타입 0x0B–0x0D)와 DTS-HD(0x11, 서브헤더 해체)가 FFmpeg로 디코딩된다. DTS:X면 객체를 렌더하지 않는다고 한 번 알린다.
  - Verify: E2E(DTS 5.1 픽스처) + 샘플 시험(성공 기준 1의 DTS, 기준 4)
  - Files: `src/decode/ffmpeg.rs`, `src/iec61937.rs`, `src/main.rs`, `tests/*`

## 3단계: TrueHD

- [ ] **T10. MAT 해체** (`src/mat.rs`)
  - Acceptance: MAT 프레임에서 TrueHD 액세스 유닛을 순서대로 꺼낸다(프레임 경계에 걸친 유닛 포함). TrueHD가 아닌 MAT(PCM)는 감지해 알린다.
  - Verify: 단위 시험 (FFmpeg로 만든 TrueHD 7.1 MAT 픽스처에서 꺼낸 유닛이 원본 .thd와 바이트 단위로 같음)
  - Files: `src/mat.rs`, `tests/fixtures/*`

- [ ] **T11. TrueHD 채널 기반** (`src/decode/truehd.rs`)
  - Acceptance: TrueHD 7.1 픽스처와 샘플이 베드로 렌더된다.
  - Verify: E2E + 샘플 시험(성공 기준 1의 TrueHD 7.1)
  - Files: `src/decode/truehd.rs`, `src/main.rs`, `tests/*`

- [ ] **T12. TrueHD Atmos 객체**
  - Acceptance: 16채널 프레젠테이션과 OAMD를 읽어 베드는 베드로, 객체는 위치·게인(보간 포함)에 따라 렌더한다.
  - Verify: 샘플 시험(성공 기준 3)
  - Files: `src/decode/truehd.rs`, `src/render.rs`, `tests/samples.rs`
  - Risk: OAMD 해석이 가장 불확실하다. 기준 3을 못 맞추면 멈추고 보고한다.

## 4단계: 마무리

- [ ] **T13. 클록·성능·문서**
  - Acceptance: 모든 경로에서 출력 길이가 입력에 대응(성공 기준 6), ARM VM에서 실시간의 3배 이상(기준 7), README의 지원 표·실행법(`--input-channels`)·샘플 결과가 실제와 일치(기준 8).
  - Verify: 샘플 시험 + 벤치마크 스크립트 결과를 README에 기록
  - Files: `tests/samples.rs`, `README.md`

## 멈추고 묻는 경우

- 시험을 통과시킬 수 없거나 빌드가 뚜렷한 해결책 없이 깨질 때
- 스펙에 없는 결정이 필요할 때 (새 의존성, OpenJOC/truehd 패치, 성공 기준 변경)
