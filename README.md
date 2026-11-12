# atmos67

HDMI로 들어온 Dolby Atmos 비트스트림을 스피커별 PCM으로 풀어 AES67로 내보내는 체인의 디코더 부분입니다. 캡처, 룸 보정, AES67 송출은 기존 도구를 그대로 씁니다.

```
arecord ─> atmos67 ─> CamillaDSP ─> RAVENNA ALSA 장치 ─> AES67 (RTP)
(HDMI)    (디코딩)    (보정 필터)    (aes67-linux-daemon)
```

## 지원 범위

| 입력 | 상태 |
|---|---|
| E-AC-3 JOC (Dolby Digital Plus Atmos), E-AC-3 | 지원. [OpenJOC](https://github.com/chyinan/OpenJOC)로 디코딩·렌더합니다. |
| TrueHD Atmos, MAT 2.0 PCM | 미지원. 건너뛰고 무음을 냅니다. |
| PCM, 일시정지 | 무음을 냅니다. |

검증은 합성 샘플을 쓴 자동 테스트까지입니다. 실제 HDMI 입력, 실시간 성능, AES67 송출은 실기에서 확인하지 않았습니다.

## 빌드

Rust 1.89 이상이 필요합니다.

```sh
cargo build --release
cargo test
```

## 스피커 배치

`--layout 7.1.4` 같은 프리셋이나 `--layout-file`로 임의 배치를 줍니다. 파일은 OpenJOC 형식이고 예시는 [`tests/fixtures/layout-5.1.2.json`](tests/fixtures/layout-5.1.2.json)에 있습니다. 출력 채널 순서는 파일에 적은 순서이며, 실행하면 stderr에 표시됩니다.

## 실행

### 1. AES67 스트림 구성

[aes67-linux-daemon](https://github.com/bondagit/aes67-linux-daemon)을 설치한 뒤, 배치에 맞는 스트림 구성을 만들어 데몬의 `status_file`로 씁니다. 8채널씩 48 kHz L24 스트림으로 나뉩니다.

```sh
atmos67 --layout-file layout.json --aes67-sources > status.json
```

### 2. 보정 없이 재생

E-AC-3는 HDMI에서 4배속 스테레오로 실려 오므로 192 kHz 2채널 16비트로 캡처합니다. `hw:HDMIIN`과 채널 수 `8`은 장치와 배치에 맞게 바꿉니다.

```sh
arecord -D hw:HDMIIN -f S16_LE -c 2 -r 192000 -t raw \
  | atmos67 --layout-file layout.json \
  | aplay -D plughw:RAVENNA -f FLOAT_LE -c 8 -r 48000 -t raw
```

이 방식은 HDMI 소스 클록과 AES67의 PTP 클록 차이를 보정하지 않아 오래 틀면 끊깁니다. 상시 사용은 아래 CamillaDSP 경로를 씁니다.

### 3. 룸 보정

[SECS 2R](https://github.com/115dkk/Sound-Equalization-and-Correction-System-2R)의 `secs2r-cli`로 측정하고 필터를 설계합니다. 측정 스윕은 RAVENNA ALSA 장치로 재생하면 같은 AES67 경로로 스피커에 나갑니다.

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
arecord -D hw:HDMIIN -f S16_LE -c 2 -r 192000 -t raw \
  | atmos67 --layout-file layout.json \
  | camilladsp <내보낸 설정.yml>
```

설정 파일은 `export/` 아래에 디자인 이름으로 만들어집니다. SECS 2R의 프리셋·채널 배정이 `atmos67`의 출력 채널 순서와 맞는지 확인해야 합니다.

## 동작

- 손상된 버스트는 버리고 다음 버스트부터 다시 디코딩합니다.
- 디코딩된 오디오가 없는 구간(일시정지, PCM, 미지원 포맷)은 무음으로 채워 출력이 캡처 클록을 따라가게 합니다.

## 성능

Apple M1의 aarch64 Linux VM(Apple `container` machine, 4코어)에서 릴리스 빌드로 163.8초 분량의 합성 E-AC-3 JOC 스트림을 처리한 시간입니다. 디코더는 스레드 하나로 돕니다.

| 배치 | 처리 시간 | 실시간 대비 |
|---|---|---|
| 5.1 | 17초 | 약 9.6배 |
| 5.1.2 (임의 배치) | 17초 | 약 9.6배 |
| 7.1.4 | 22초 | 약 7.4배 |
| 9.1.6 | 20초 | 약 8.2배 |

초 단위로 한 번씩 잰 값이라 오차가 큽니다. RK3588 실기 측정값이 아니며, RK3588의 코어는 M1보다 느리므로 여유는 이보다 작습니다. CamillaDSP의 필터 연산 부하는 포함하지 않았습니다.

## 제약

- **HDCP**: 보호된 신호는 다루지 않습니다. HDCP 키가 없는 수신기에는 보호 소스가 신호를 보내지 않으며, 이 프로그램은 캡처 장치가 받아 ALSA로 내주는 오디오만 처리합니다.
- **HDMI 입력 하드웨어**: 보드에 HDMI 입력 포트와 그 ALSA 캡처 드라이버가 있어야 합니다. Orange Pi 5 계열은 모델마다 HDMI 입력 유무가 다르므로 확인이 필요합니다.
- **비트 단위 캡처**: HDMI 입력 드라이버가 192 kHz 스테레오 16비트를 변형 없이(리샘플·볼륨·믹싱 없이) 내줘야 비트스트림이 살아남습니다. 실기에서 확인하지 않았습니다.
- **EDID**: 소스가 Atmos를 보내려면 HDMI 입력의 EDID가 E-AC-3 JOC 지원을 알려야 합니다.
