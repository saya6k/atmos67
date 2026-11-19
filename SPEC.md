# Spec: atmos67 — 모든 HDMI 오디오 포맷을 스피커별 PCM으로

## Objective

`atmos67`은 HDMI 캡처(IEC 61937 비트스트림이 실린 PCM)를 stdin으로 받아, 설정한 임의 스피커 배치에 맞춘 48 kHz FLOAT32LE PCM을 stdout으로 내보내는 필터다. 뒤에 CamillaDSP(룸 보정)와 aes67-linux-daemon(AES67 송출)이 붙는다. 사용자는 Orange Pi(RK3588) 한 대로 이 체인을 돌리려는 개인 사용자다.

> **범위 변경 (2026-11-17).** 실행 대상이 OSMC Vero V로 바뀌었다(`tasks/vero-v.md`). AES67 송출은 atmos67이 직접 하고(`--aes67-send`), 룸 보정·베이스 매니지먼트·볼륨도 CamillaDSP 대신 atmos67이 SECS 2R 엔진으로 맡는다(`tasks/dsp.md`). 아래 본문은 디코딩 범위의 원래 스펙이다.

지금은 E-AC-3 JOC만 디코딩되고 나머지는 무음이다. 이 스펙의 목표는 **HDMI로 올 수 있는 포맷마다 소리가 맞는 스피커에서 나게 하는 것**이다.

| IEC 61937 타입 | 포맷 | 디코더 | 렌더 |
|---|---|---|---|
| 0x01 | AC-3 | FFmpeg | 채널 베드 |
| 0x15 | E-AC-3 JOC (Atmos) | OpenJOC (현행) | OpenJOC |
| 0x15 | E-AC-3 (JOC 없음), OpenJOC가 거부한 JOC 스트림 | FFmpeg | 채널 베드 |
| 0x16 | MAT: TrueHD (Atmos 아님) | `truehd` 크레이트 | 채널 베드 |
| 0x16 | MAT: TrueHD Atmos | `truehd` 크레이트 (16채널 + OAMD) | 객체 |
| 0x0B–0x0D, 0x11 | DTS, DTS-HD (HRA/MA) | FFmpeg | 채널 베드 |
| 0x11 | DTS:X | FFmpeg (7.1 베드까지) | 채널 베드. 객체는 범위 밖 |
| 0x16 | MAT 2.0 PCM | 없음 | 범위 밖. 감지해 알리고 무음 |

조사 결과 DTS:X 객체와 MAT 2.0 PCM은 쓸 수 있는 오픈소스 디코더를 찾지 못했다(FFmpeg는 DTS:X를 감지만 하고 7.1 베드로 디코딩한다). 둘은 이 스펙의 범위 밖이다.

### 가정

1. 출력은 항상 48 kHz다. 48 kHz가 아닌 비트스트림 콘텐츠(96/192 kHz TrueHD, 44.1 kHz DTS)는 "미지원"으로 알리고 무음을 낸다. 스테레오 PCM은 FFmpeg의 libswresample로 48 kHz로 바꾼다(2026-11-19).
2. 캡처 형식은 실행 중 바뀌지 않는다. 사용자가 `arecord`로 고정하고 `atmos67`에 `--input-channels`(2 또는 8), `--input-rate`(기본 192000), `--input-format`(`S16_LE` 기본, `S32_LE`, `FLOAT_LE`)으로 알려 준다. 16비트가 아닌 형식은 스테레오 PCM만 담는다. 8채널 캡처에서는 8채널 전체에 실린 버스트(HBR: TrueHD, DTS-HD)와 앞 2채널에만 실린 버스트(AC-3, E-AC-3, DTS)를 모두 찾는다.
3. 스테레오 캡처의 PCM(Kodi가 직접 재생하는 음악과 메뉴 효과음)은 FL, FR 베드로 렌더해 보정을 함께 받는다(2026-11-19, `src/pcm.rs`). 비트스트림을 PCM으로 오인해 스피커로 내보내는 사고를 막기 위해, 16비트 캡처는 IEC 61937 동기 워드가 50 ms 넘게 없어야 PCM으로 보고, 비트스트림이 끝난 직후의 50 ms(마지막 버스트의 나머지가 들어 있다)는 내보내지 않는다. 8채널 캡처의 PCM과 멀티채널 PCM은 지금처럼 무음이다(Kodi가 멀티채널을 Dolby Digital로 트랜스코딩해 보낸다).
4. FFmpeg는 libavcodec을 `ffmpeg-next` 크레이트로 링크해 쓴다(별도 프로세스가 아니라 패킷 단위 동기 디코딩). 빌드와 실행 환경에 FFmpeg 라이브러리가 필요해진다.
5. LFE 채널은 배치에 `lfe` 역할 스피커가 있으면 그리로 보내고, 없으면 버린다. 베이스 매니지먼트는 송출기의 DSP가 맡는다(`tasks/dsp.md`).

### 렌더러

자체 VBAP 렌더러 하나를 베드와 객체가 함께 쓴다.

- 스피커 배치는 지금 쓰는 OpenJOC JSON 형식(방위각·고도·역할)을 그대로 읽는다.
- 베드 채널은 표준 위치의 정지 음원으로 다룬다. 배치에 그 위치의 스피커가 있으면(각도 차 5° 이내) 그 스피커 하나로만 보낸다.
- 객체는 OAMD의 위치와 게인을 따르고, 메타데이터 갱신 사이는 보간한다.
- 게인은 파워 보존(제곱합 1)으로 정규화한다.

## Tech Stack

- Rust 1.99+(`rust-toolchain.toml`은 1.100.0), edition 2024, 단일 바이너리 크레이트
- `openjoc-api`, `openjoc-scene` (git, 커밋 고정) — E-AC-3 JOC, 배치 파일 파싱
- `truehd` (crates.io) — TrueHD 디코딩과 OAMD
- `ffmpeg-next` (crates.io) + 시스템 FFmpeg 라이브러리 — AC-3, E-AC-3, DTS 계열, 스테레오 PCM 리샘플(libswresample)
- SECS 2R 공개 크레이트 (crates.io, AGPL-3.0-only) — 측정과 보정 계산 (`tasks/dsp.md`)

## Commands

```sh
cargo build --release
cargo test                      # 단위 + 합성 픽스처 E2E
cargo test -- --ignored         # samples/ 의 실제 파일을 쓰는 시험 (samples/ 가 있어야 함)
cargo clippy --all-targets -- -D warnings
```

이 Mac에는 Rust가 없어 `linux-test` 컨테이너 머신의 `rust:alpine` 이미지에서 돌린다(`apk add ffmpeg-dev clang-dev pkgconf` 필요). 래퍼 스크립트는 `target/` 아래에 두고 커밋하지 않는다.

## Project Structure

```
src/main.rs          인자 처리, 입력 루프, 캡처 클록에 맞춘 출력
src/iec61937.rs      IEC 61937 버스트 추출 (현행)
src/mat.rs           MAT 프레임 해체 → TrueHD 액세스 유닛 / MAT PCM 감지
src/layout.rs        스피커 배치와 표준 채널 위치
src/render.rs        VBAP 렌더러 (베드, 객체)
src/decode/joc.rs    OpenJOC 세션 (현행 코드를 옮김)
src/decode/ffmpeg.rs libavcodec 디코더 (AC-3, E-AC-3, DTS)
src/decode/truehd.rs truehd 크레이트 디코더와 OAMD → 객체
src/aes67.rs         aes67-linux-daemon 소스 구성 출력 (현행)
src/send.rs          사용자 공간 AES67 송출 (RTP + SAP)
tests/e2e.rs         합성 픽스처 E2E
tests/send.rs        송출기 E2E
tests/samples.rs     samples/ 기반 시험 (#[ignore])
tests/fixtures/      FFmpeg로 만든 작은 합성 스트림과 배치 파일
samples/             실제 테스트 파일 (git 제외)
```

## Code Style

현행 코드를 따른다. 모듈 머리에 그 모듈이 다루는 형식을 한 문단으로 설명하고, 주석은 코드로 알 수 없는 이유만 적는다. 추상화는 두 번째 쓰임이 생길 때 만든다.

```rust
/// Returns the next complete burst as (data type, payload in bitstream
/// byte order), or `None` when more input is needed.
pub fn next_burst(&mut self) -> Option<(u8, &[u8])> {
```

- 오류: `main`은 `Box<dyn Error>`, 스트림 오류는 끝내지 않고 stderr에 한 번 알린 뒤 무음으로 이어 간다.
- stderr 메시지는 `atmos67: ` 접두어, 같은 내용은 반복하지 않는다.
- `unsafe` 없음. clippy 경고 없음.

## Testing Strategy

- **단위 시험** (`#[cfg(test)]`, 각 모듈): 버스트·MAT 해체, VBAP 게인(스피커 위치에서 단일 스피커, 파워 보존, 스피커 사이에서 두 스피커), 채널 위치 표.
- **합성 E2E** (`tests/e2e.rs`): FFmpeg로 채널마다 다른 주파수의 사인파를 인코딩해 IEC 61937에 담은 작은 픽스처(AC-3 5.1, E-AC-3 5.1, TrueHD 7.1, DTS 5.1). 같은 배치로 렌더했을 때 각 주파수가 맞는 출력 채널에서만 나오는지 본다.
- **실제 샘플 시험** (`tests/samples.rs`, `#[ignore]`): `samples/`가 없으면 건너뛴다. 아래 성공 기준을 그대로 검사한다.
- 작업마다 실패하는 시험을 먼저 쓰고, 통과시키고, 전체 시험을 돌린 뒤 커밋한다.

## Boundaries

- **Always**: 커밋 전 전체 시험 통과. 작업 하나에 커밋 하나, 그 작업이 건드린 파일만 스테이징. 지원 범위가 바뀌면 README 갱신. 디코딩하지 못한 구간은 무음.
- **Ask first**: 위 Tech Stack 밖의 의존성 추가. OpenJOC나 truehd를 포크하거나 패치하는 일. 출력 형식·CLI의 호환을 깨는 변경. 이 스펙의 범위 변경.
- **Never**: HDCP·DRM 우회. 디코딩되지 않은 비트스트림을 오디오로 출력. 캡처 데이터는 가정 3의 조건으로 PCM이라고 판단한 것만 출력한다. `samples/` 커밋. `samples/`의 설치 파일(.pkg, .msi) 실행.

## Success Criteria

`samples/Audio Test Files/`의 파일을 FFmpeg로 IEC 61937에 담아 넣었을 때:

1. **채널 기반 포맷은 같은 배치에서 원본과 같다.** `Dolby Digital AC-3 5.1`, `Dolby Digital Plus E-AC-3 5.1`/`7.1`, `Dolby TrueHD 7.1`, `DTS-HD MA 5.1`/`7.1`, `DTS 5.1`(48 kHz인 것)을 원본 채널 구성과 같은 프리셋(`5.1`, `7.1`)으로 렌더한 출력이, FFmpeg로 직접 디코딩한 PCM과 채널별 RMS 차 0.5 dB 이내이고 다른 채널로 새는 양이 −60 dB 이하다.
2. **대체 디코딩.** `Atmos test tones 1000Hz V4`(OpenJOC 거부)가 무음이 아니라 E-AC-3 코어의 채널 베드로 나온다.
3. **TrueHD Atmos.** `Dolby Atmos TrueHD, E-AC-3 7.1.4.mkv`의 TrueHD 트랙을 `7.1.4`로 렌더했을 때 천장 스피커 4개에서 소리가 나고(7.1 베드만 나오는 것이 아님), 1초 단위로 가장 큰 스피커가 같은 파일의 E-AC-3 JOC 트랙 렌더와 80% 이상 일치한다.
4. **DTS:X와 MAT PCM.** DTS:X 샘플은 7.1 베드로 나오고, 객체를 렌더하지 않는다는 사실을 stderr에 한 번 알린다.
5. **기존 동작 유지.** 현재 시험 10개가 계속 통과하고, 이미 디코딩되던 E-AC-3 JOC 샘플 6개의 출력이 바뀌지 않는다.
6. **캡처 클록.** 모든 경로에서 출력 프레임 수가 입력 길이에 대응한다(현행 무음 채움 규칙 유지, 오차 2 액세스 유닛 이내).
7. **성능.** 모든 경로가 `linux-test` ARM VM에서 스레드 하나로 실시간의 3배 이상.
8. README의 지원 범위 표와 실제 샘플 시험 절이 결과와 일치한다.

## Open Questions

1. ~~**라이선스.**~~ GPL-3.0-only로 정했다(2026-11-17, `LICENSE`). FFmpeg(LGPL)와 SECS 2R 크레이트(AGPL-3.0-only)는 GPLv3와 함께 배포할 수 있다.
2. **기준 3의 80%.** 두 트랙의 믹스가 같다는 가정이다. 샘플을 돌려 보고 가정이 틀리면 기준을 다시 정한다.
3. **MAT 2.0 PCM.** 실물 캡처가 없어 감지 로직도 FFmpeg가 만든 스트림으로는 시험할 수 없다. 보드에서 캡처한 파일이 생기면 다시 다룬다.
4. **48 kHz가 아닌 콘텐츠.** 지금은 무음 처리다. 필요하면 리샘플을 별도 작업으로 추가한다.
