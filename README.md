# atmos67

HDMI로 들어온 오디오 비트스트림을 스피커별 PCM으로 풀어 AES67로 내보내는 체인의 디코더 부분입니다. 캡처, 룸 보정, AES67 송출은 기존 도구를 그대로 씁니다.

```
arecord ─> atmos67 ─> CamillaDSP ─> RAVENNA ALSA 장치 ─> AES67 (RTP)
(HDMI)    (디코딩)    (보정 필터)    (aes67-linux-daemon)
```

## 지원 범위

| 포맷 | 상태 | 디코더 |
|---|---|---|
| E-AC-3 JOC (Dolby Digital Plus Atmos) | 객체 렌더 | [OpenJOC](https://github.com/chyinan/OpenJOC) |
| E-AC-3 (Atmos 아님), OpenJOC가 거부한 Atmos 스트림 | 채널 베드 | FFmpeg |
| AC-3 | 채널 베드 | FFmpeg |
| TrueHD Atmos | 객체 렌더 | [truehd](https://github.com/truehdd/truehdd) + 자체 렌더러 |
| TrueHD | 채널 베드 | truehd |
| DTS, DTS-HD High Resolution, DTS-HD Master Audio | 채널 베드 | FFmpeg |
| DTS:X | 7.1 베드만. 객체는 렌더하지 않고 그 사실을 알립니다. | FFmpeg |
| MAT 2.0 PCM (Apple TV 등의 Atmos), LPCM | 미지원. 무음을 냅니다. | |
| 48 kHz가 아닌 콘텐츠 | 미지원. 무음을 냅니다. | |

채널 베드는 채널마다 같은 이름의 스피커로, 없으면 표준 위치에서 5° 이내의 스피커로, 그것도 없으면 주변 스피커로 패닝(VBAP)해 보냅니다. LFE는 `lfe` 역할 스피커로만 가고, 없으면 버립니다.

실제 HDMI 입력과 AES67 송출은 실기에서 확인하지 않았습니다. 아래 "시험"의 결과는 모두 파일을 FFmpeg로 IEC 61937에 담아 넣은 것입니다.

## 빌드

Rust 1.95(`rust-toolchain.toml`로 고정)와 FFmpeg 개발 라이브러리, clang이 필요합니다.

```sh
apk add ffmpeg-dev clang-dev pkgconf musl-dev   # Alpine (FFmpeg 8.0에서 확인)

cargo build --release
cargo test
cargo test --release -- --ignored    # samples/ 의 실제 파일로 하는 시험 (ffmpeg 명령 필요)
```

- musl(Alpine)에서는 `RUSTFLAGS="-C target-feature=-crt-static"`이 필요합니다.
- rustc가 `truehd` 크레이트를 최적화하려면 메모리가 10 GB 넘게 들어서, 이 크레이트만 최적화 없이 빌드합니다(`Cargo.toml`). rustc 1.99는 최적화 없이도 메모리가 부족해집니다.

## 스피커 배치

`--layout 7.1.4` 같은 프리셋이나 `--layout-file`로 임의 배치를 줍니다. 파일은 OpenJOC 형식이고 예시는 [`tests/fixtures/layout-5.1.2.json`](tests/fixtures/layout-5.1.2.json)에 있습니다.

- **방위각은 왼쪽이 양수입니다.** 정면이 0, 왼쪽 30°에 있는 스피커는 `30`, 오른쪽은 `-30`입니다. 고도는 위가 양수입니다.
- 출력 채널 순서는 파일에 적은 순서이며, 실행하면 stderr에 표시됩니다.

## 실행

### 1. AES67 스트림 구성

[aes67-linux-daemon](https://github.com/bondagit/aes67-linux-daemon)을 설치한 뒤, 배치에 맞는 스트림 구성을 만들어 데몬의 `status_file`로 씁니다. 8채널씩 48 kHz L24 스트림으로 나뉩니다.

```sh
atmos67 --layout-file layout.json --aes67-sources > status.json
```

### 2. 캡처 형식

HDMI는 포맷에 따라 다른 형식으로 비트스트림을 싣습니다. `atmos67`은 캡처 형식을 스스로 알 수 없으므로 `arecord`에 준 것과 같은 값을 알려 줘야 합니다.

| 소스가 보내는 포맷 | 캡처 | `atmos67` 옵션 |
|---|---|---|
| TrueHD, DTS-HD (HBR 모드) | 8채널 192 kHz | `--input-channels 8` |
| E-AC-3 | 2채널 192 kHz | (기본값) |
| AC-3, DTS | 2채널 48 kHz | `--input-rate 48000` |

8채널 192 kHz 캡처는 8채널 전체에 실린 스트림과 앞 2채널에만 실린 스트림을 모두 찾으므로, 캡처 장치가 형식을 고정해 줄 수 있다면 이 한 가지로 E-AC-3까지 받을 수 있습니다. 소스가 HDMI 전송 속도를 바꾸는 포맷(AC-3, DTS)은 캡처를 다시 시작해야 합니다.

### 3. 보정 없이 재생

`hw:HDMIIN`과 채널 수 `8`은 장치와 배치에 맞게 바꿉니다.

```sh
arecord -D hw:HDMIIN -f S16_LE -c 8 -r 192000 -t raw \
  | atmos67 --layout-file layout.json --input-channels 8 \
  | aplay -D plughw:RAVENNA -f FLOAT_LE -c 8 -r 48000 -t raw
```

이 방식은 HDMI 소스 클록과 AES67의 PTP 클록 차이를 보정하지 않아 오래 틀면 끊깁니다. 상시 사용은 아래 CamillaDSP 경로를 씁니다.

### 4. 룸 보정

[SECS 2R](https://github.com/115dkk/Sound-Equalization-and-Correction-System-2R)의 `secs2r-cli`로 측정하고 필터를 설계합니다. 측정 스윕은 RAVENNA ALSA 장치로 재생하면 같은 AES67 경로로 스피커에 나갑니다. 이 절의 명령은 CLI 인자 정의를 보고 적은 것이고 실행해 확인하지는 않았습니다.

```sh
secs2r-cli project new room.secs --preset 5.1
secs2r-cli measure room.secs --position 1 --method native
secs2r-cli calibrate room.secs
secs2r-cli export room.secs --kind camilla-dsp --out export/
```

SECS 2R이 내보낸 CamillaDSP 설정은 장치가 파일 자리표시자로 돼 있습니다. `devices` 아래의 `capture`, `playback`을 다음으로 바꾸고 클록 보정 항목을 더합니다(CamillaDSP v4 문법).

```yaml
devices:
  samplerate: 48000
  enable_rate_adjust: true
  resampler:
    type: AsyncSinc
    profile: Balanced
  capture:
    type: Stdin
    channels: 8
    format: F32_LE
  playback:
    type: Alsa
    channels: 8
    device: "hw:RAVENNA"
    format: S32_LE
```

```sh
arecord -D hw:HDMIIN -f S16_LE -c 8 -r 192000 -t raw \
  | atmos67 --layout-file layout.json --input-channels 8 \
  | camilladsp <내보낸 설정.yml>
```

설정 파일은 `export/` 아래에 디자인 이름으로 만들어집니다. SECS 2R의 프리셋·채널 배정이 `atmos67`의 출력 채널 순서와 맞는지 확인해야 합니다.

## 동작

- 손상된 버스트는 버리고 다음 버스트부터 다시 디코딩합니다.
- 디코딩된 오디오가 없는 구간(일시정지, PCM, 미지원 포맷)은 무음으로 채워 출력이 캡처 클록을 따라가게 합니다.
- E-AC-3는 버스트마다 OpenJOC에 먼저 주고, 거부하면 그 버스트의 채널 베드를 냅니다. 네 번 연속 거부하면 약 1초에 한 번만 다시 시도합니다.
- TrueHD Atmos의 객체는 OAMD의 위치와 게인을 따르고, 갱신 사이는 지정된 길이로 보간합니다. 객체 크기, 스냅, 구역 제한, 트림은 반영하지 않습니다. 갱신 블록의 시작 시점 단위(32 샘플)는 규격을 확인하지 못한 추정입니다.

## 시험

`cargo test`는 FFmpeg로 만든 합성 픽스처(채널마다 다른 주파수의 사인파)로 각 채널이 맞는 스피커로 가는지 봅니다. `cargo test --release -- --ignored`는 `samples/`의 공개 테스트 파일로 다음을 확인합니다.

| 샘플 | 결과 |
|---|---|
| AC-3 5.1, E-AC-3 5.1·7.1, TrueHD 7.1, DTS-HD MA 5.1·7.1, DTS-HD HRA 7.1 | 같은 배치로 렌더한 출력이 FFmpeg의 디코딩과 샘플 단위로 같고, 출력 길이가 캡처 길이와 같습니다. |
| DTS:X 7.1.4 | 7.1 베드가 FFmpeg의 디코딩과 같고, 객체를 렌더하지 않는다고 한 번 알립니다. |
| Atmos test tones 1000Hz V4 (OpenJOC가 거부) | E-AC-3 코어의 5.1 베드가 FFmpeg의 디코딩과 같습니다. |
| TrueHD Atmos 7.1.4 (스피커를 1분씩 호명) | 12개 스피커 모두 호명된 스피커에서만 소리가 나고(다른 스피커는 80 dB 이상 낮음), 같은 파일의 E-AC-3 JOC 트랙 렌더와 가장 큰 스피커가 12/12 일치합니다. |
| E-AC-3 JOC 5.1.2 | 첫 액세스 유닛만 OpenJOC가 거부해 베드로 나오고, 나머지는 객체로 디코딩됩니다. |

확인하지 못한 것:

- TrueHD Atmos 시험 샘플의 객체는 모두 스피커 위치에 고정돼 있습니다. 움직이는 객체와 방 안쪽에 놓인 객체는 단위 시험으로만 확인했고, 실제 콘텐츠로는 확인하지 못했습니다.
- E-AC-3 JOC에서 객체로 실린 채널은 OpenJOC가 맞는 스피커를 가장 크게 내지만 이웃 스피커에도 2~3 dB 낮게 함께 냅니다.

## 성능

Apple M1의 aarch64 Linux VM(Apple `container` machine)에서 릴리스 빌드로 실제 샘플 60초를 `--layout 7.1.4`로 처리한 시간입니다. 스레드 하나로 돕니다. 초 단위로 한 번씩 잰 값입니다.

| 포맷 | 처리 시간 | 실시간 대비 |
|---|---|---|
| AC-3 5.1 (48.6초 분량) | 약 1초 | 40배 이상 |
| E-AC-3 7.1 | 약 1초 | 60배 안팎 |
| DTS-HD MA 7.1 | 약 1초 | 60배 안팎 |
| TrueHD 7.1 | 23초 | 약 2.6배 |
| TrueHD Atmos 7.1.4 | 35초 | 약 1.7배 |
| E-AC-3 JOC 7.1.4 (천장 객체 구간) | 30초 | 약 2.0배 |

아래 세 경로는 여유가 작습니다. RK3588의 코어는 M1보다 느리므로 실기에서 실시간을 유지하지 못할 수 있습니다. TrueHD는 `truehd` 크레이트를 최적화 없이 빌드하기 때문이고, E-AC-3 JOC는 OpenJOC의 처리량입니다. CamillaDSP의 필터 연산 부하는 포함하지 않았습니다.

## 제약

- **HDCP**: 보호된 신호는 다루지 않습니다. HDCP 키가 없는 수신기에는 보호 소스가 신호를 보내지 않으며, 이 프로그램은 캡처 장치가 받아 ALSA로 내주는 오디오만 처리합니다.
- **HDMI 입력 하드웨어**: 보드에 HDMI 입력 포트와 그 ALSA 캡처 드라이버가 있어야 합니다. Orange Pi 5 계열은 모델마다 HDMI 입력 유무가 다르므로 확인이 필요합니다.
- **비트 단위 캡처**: HDMI 입력 드라이버가 캡처 데이터를 변형 없이(리샘플·볼륨·믹싱 없이) 내줘야 비트스트림이 살아남습니다. 실기에서 확인하지 않았습니다.
- **EDID**: 소스가 원하는 포맷을 보내려면 HDMI 입력의 EDID가 그 포맷 지원을 알려야 합니다.
