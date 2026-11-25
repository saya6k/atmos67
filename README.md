# atmos67

오디오 비트스트림(IEC 61937)을 임의의 스피커 배치에 맞춘 스피커별 PCM으로 풀고, 룸 보정을 거쳐 AES67로 내보냅니다. 수신기는 AES67 모드의 Dante 스피커·앰프 등입니다. AES67 카드는 필요 없습니다. 보통 이더넷으로 송출합니다.

쓰는 방법은 두 가지입니다.

1. **OSMC(Vero V)의 Kodi와 함께 (주 대상, 패키지와 Kodi 애드온 제공).** Kodi가 비트스트림을 ALSA 루프백으로 패스스루하면, atmos67이 그것을 디코딩합니다. 그다음 보정(SECS 2R 엔진 내장), 볼륨 조절, AES67 송출(RTP·SAP·PTP)까지 직접 합니다. 아래 "OSMC (Vero V) 패키지"를 봅니다.
2. **HDMI 입력이 있는 보드에서 기존 도구와 함께.** `arecord`로 받은 HDMI 캡처를 디코딩만 하고, 그 뒤 보정은 CamillaDSP가, 송출은 aes67-linux-daemon이 맡습니다. 아래 "실행"을 봅니다. 처음 구상했던 구성이고, 보드에서는 시험하지 않았습니다.

```
arecord ─> atmos67 ─> CamillaDSP ─> RAVENNA ALSA 장치 ─> AES67 (RTP)
(HDMI)    (디코딩)    (보정 필터)    (aes67-linux-daemon)
```

**지원하는 장비.** 패키지(`.deb`)와 애드온은 Vero V의 OSMC(Debian bullseye armhf, Kodi 21) 전용입니다. `atmos67` 바이너리 자체는 ALSA 캡처 스트림만 받으면 되는 일반 Linux 프로그램입니다. 지금까지 aarch64 Alpine(musl)과 armv7 bullseye에서 시험했습니다. 다른 Linux 기반 Kodi에서도 같은 구성(`snd-aloop` → atmos67)이 가능해 보이지만 확인하지 않았습니다. 해 보려면 커널에 `snd-aloop` 모듈이 있어야 하고, 서비스와 애드온은 그 배포판에 맞춰 다시 만들어야 합니다. Windows, Android, macOS의 Kodi는 지원하지 않습니다. ALSA와 Linux 전용 기능을 쓰기 때문입니다.

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
| 스테레오 PCM (Kodi가 직접 재생하는 음악, 메뉴 효과음) | 앞 스피커 두 개(FL, FR)로. 보정과 베이스 매니지먼트가 함께 적용됩니다. 16비트·32비트·float, 48 kHz가 아니면 48 kHz로 바꿉니다. | FFmpeg (libswresample) |
| MAT 2.0 PCM (Apple TV 등의 Atmos), 멀티채널 LPCM | 미지원. 무음을 냅니다. | |
| 48 kHz가 아닌 비트스트림 | 미지원. 무음을 냅니다. | |

채널 베드는 채널마다 같은 이름의 스피커로, 없으면 표준 위치에서 5° 이내의 스피커로, 그것도 없으면 주변 스피커로 패닝(VBAP)해 보냅니다. LFE는 `lfe` 역할 스피커로만 가고(여럿이면 N대가 1/N씩 나눠 합이 하나와 같습니다), 없으면 버립니다.

실제 HDMI 입력과 AES67 송출은 실기에서 확인하지 않았습니다. 아래 "시험"의 결과는 모두 파일을 FFmpeg로 IEC 61937에 담아 넣은 것입니다.

## 빌드

Rust 1.100(`rust-toolchain.toml`로 고정)와 FFmpeg 개발 라이브러리, clang이 필요합니다.

```sh
apk add ffmpeg-dev clang-dev pkgconf musl-dev   # Alpine (FFmpeg 8.0, 8.1.3에서 확인)

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

HDMI 입력이 있는 보드에서 aes67-linux-daemon과 함께 쓰는 방법입니다. Kodi가 설치된 OSMC(Vero V)에서는 아래 "OSMC (Vero V) 패키지"를 봅니다.

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

## OSMC (Vero V) 패키지

Kodi가 재생하는 영상의 오디오를 같은 장비에서 디코딩해 AES67로 보내는 구성입니다. Vero V에는 HDMI 입력이 없으므로 Kodi가 비트스트림을 ALSA 루프백 장치로 패스스루하고, `atmos67`이 그것을 캡처합니다.

```
Kodi ─> ALSA 루프백 ─> arecord ─> atmos67 ─> atmos67 --aes67-send ─> AES67 (RTP)
(패스스루) (snd-aloop)            (디코딩)    (송출)                    ptp4l (PTP)
```

Vero V의 커널은 서명된 모듈만 받기 때문에(`CONFIG_MODULE_SIG_FORCE`) aes67-linux-daemon의 RAVENNA 커널 모듈을 올릴 수 없습니다. 그래서 송출은 `atmos67 --aes67-send`가 직접 합니다. 8채널씩 48 kHz L24, 1 ms 패킷의 멀티캐스트 RTP 스트림을 보내고 SAP로 알리며, 재생하지 않는 동안에는 무음을 보냅니다. RTP 타임스탬프는 시스템 클록에서 읽은 PTP 시간이고, 시스템 클록은 linuxptp(`ptp4l`, 하드웨어 타임스탬프가 있으면 `phc2sys`도)가 PTP에 맞춥니다.

### 빌드

Docker가 있는 Linux(arm64 또는 amd64)에서 OSMC의 userland(Debian bullseye, armhf)용으로 교차 빌드합니다. FFmpeg는 필요한 디코더(AC-3, E-AC-3, DTS)만 넣어 정적으로 링크합니다.

```sh
docker build -t atmos67-armhf packaging
docker run --rm -v "$PWD":/work atmos67-armhf packaging/build.sh
```

시험이 armhf 바이너리를 실행하므로 커널에 32비트 ARM용 qemu가 등록돼 있어야 합니다(Docker Desktop은 돼 있고, 아니면 `docker run --privileged --rm tonistiigi/binfmt --install arm`).

`target/packages/`에 `atmos67_<버전>_armhf.deb`와 `service.atmos67-<버전>.zip`이 생깁니다.

### 설치

```sh
sudo apt install ./atmos67_0.1.0_armhf.deb     # Vero V에서. alsa-utils, linuxptp, ethtool을 함께 설치합니다.
```

패키지는 `snd-aloop` 모듈을 올리고 서비스 세 개를 켭니다: `atmos67`(캡처·디코딩·송출), `atmos67-ptp4l`, `atmos67-phc2sys`. 그 다음 Kodi에서:

1. 설정 → 애드온 → zip 파일에서 설치로 `service.atmos67-0.1.0.zip`을 설치합니다.
2. 애드온 설정에서 스피커 배치 프리셋, 네트워크 인터페이스, 멀티캐스트 주소, PTP 도메인을 고릅니다. 바꾸면 서비스가 다시 시작됩니다. 실제 위치와 거리는 측정 보정이 맞추므로 배치 파일은 애드온에서 고르지 않습니다(설정 파일의 `LAYOUT_FILE`로는 쓸 수 있습니다).
   - "스트림 구성"에서 어느 스피커를 어느 스트림(이름, 멀티캐스트 주소, 최대 8채널)에 넣을지 정합니다. 수신기마다 필요한 채널만 받게 할 수 있고, 스트림 이름이 수신기(예: Dante Controller)에 보이는 이름입니다. 정하지 않으면 순서대로 8채널씩 보냅니다. 고르는 목록은 지금 보정 모드에서 실제로 보내는 채널입니다. "끔"이면 렌더한 스피커(LFE 포함), 그 밖에는 보정 설계의 출력(메인과 `SW1`…)입니다. 모드를 바꾸면 없는 채널 이름은 송출기가 알리고 건너뜁니다.
   - "상태 보기"는 서비스, 보정 모드, PTP 그랜드마스터, 재생 여부, 볼륨, 지연, 송출 오류, 스트림 구성을 보여 줍니다.
   - "Dante 수신기 연결"은 네트워크의 Dante 장비를 찾아 AES67 모드를 켜거나 끄고(장비 재부팅 후 적용), 스트림을 수신기의 N번 채널부터 차례로 구독시킵니다. Dante Controller에서 하는 일과 같으며, 명령줄로는 `atmos67 dante list | aes67 | connect`입니다. Dante의 제어 프로토콜은 비공개라 [netaudio](https://github.com/chris-ritsen/network-audio-controller)의 역분석 구현(`netaudio-core`, Unlicense)을 씁니다. **Dante 장비에서는 아직 시험하지 못했습니다.** 동작하지 않으면 Dante Controller로 같은 이름의 스트림을 구독하면 됩니다. Dante 장비는 구독을 저장해 두므로 한 번이면 됩니다. 다만 스트림 구성을 바꾸거나 Vero의 IP 주소가 바뀌면 다시 구독해야 합니다. AES67 모드를 바꾼 뒤에는 설정을 다시 읽어 확인하는데, 장비가 설정을 알려 주지 않으면 실패 대신 `"confirmed": false`를 냅니다(명령은 보냈습니다).
3. 애드온 설정의 "Kodi 오디오 출력을 atmos67로 설정"을 실행합니다. 오디오 출력과 패스스루 장치를 `atmos67 (AES67)`로 바꾸고, 스피커 수를 2.0으로, 모든 포맷의 패스스루와 Dolby Digital 트랜스코딩을 켭니다. Kodi를 다시 시작해야 장치가 목록에 나타날 수 있습니다.

애드온 없이 쓰려면 `/etc/atmos67/atmos67.conf`를 고치고 `sudo systemctl restart atmos67`을 합니다.

### 보정, 베이스 매니지먼트, 볼륨

송출기가 보내기 전에 스피커를 보정합니다. 애드온의 "베이스 매니지먼트·레벨" 범주에서 모드를 고릅니다.

| 모드 | 하는 일 |
|---|---|
| 끔 | 렌더한 스피커를 그대로 보냅니다(LFE 채널 포함). |
| 수동 | 스피커 그룹(프런트, 센터, 서라운드, 백, 천장)마다 크로스오버(Linkwitz–Riley 12/24 dB)를 걸어 저역과 LFE(+10 dB)를 서브우퍼로 보내고, 스피커별 레벨과 거리를 적용합니다. 서브우퍼가 여럿이면 똑같이 나눠 냅니다. |
| 측정 보정 | 보정 마법사로 측정해 [SECS 2R](https://github.com/115dkk/Sound-Equalization-and-Correction-System-2R)이 설계한 보정(EQ, FIR, 레벨, 지연, 크로스오버, 서브우퍼 정렬)에 스피커별 레벨만 더해 적용합니다. |

- 보정이 켜져 있으면 서브우퍼가 출력 채널 끝에 붙습니다(7.1.4와 서브 2대면 메인 11 + 서브 2 = 13채널, 스트림 8 + 5). 보정은 1024프레임 블록으로 처리해 약 21 ms의 지연이 더해지고, 합산이 클리핑되지 않도록 10 dB 낮게 나갑니다.
- **보정 마법사**("룸 보정" 범주): USB 측정 마이크(ALSA 장치, 예 `plughw:1,0`), 마이크 보정 파일, 측정 위치 수를 정하고 "보정 시작"을 누릅니다. 서비스를 측정 모드로 다시 시작한 뒤 위치마다 모든 스피커에서 스윕을 재생하고, 끝나면 SECS 2R로 보정을 설계해 측정 보정 모드로 바꿉니다. 설계는 몇 분 걸립니다. 같은 일을 명령으로도 할 수 있습니다.

  ```sh
  atmos67 calibrate start --layout 7.1.4 --subs 2 --project DIR --control /run/atmos67/control --mic plughw:1,0
  atmos67 calibrate measure --project DIR --control /run/atmos67/control --mic plughw:1,0 --position 1
  atmos67 calibrate finish --project DIR --control /run/atmos67/control --mic plughw:1,0 --out DESIGN_DIR
  ```

- **볼륨**: 패스스루 중에는 Kodi가 소리를 줄이지 않으므로, 애드온이 Kodi의 볼륨 변경을 송출기에 보냅니다. 100 %가 "최대 볼륨"이고 그 아래 60 dB를 씁니다. 바뀐 볼륨은 설정 파일에도 적어 두므로, 서비스가 스스로 다시 시작해도(그랜드마스터 변경 등) 그 볼륨으로 켜집니다. Kodi가 직접 재생하는 스테레오 PCM은 Kodi가 이미 볼륨을 적용했으므로, 송출기는 그 구간에 볼륨과 음소거를 다시 걸지 않습니다. 디코더가 출력 끝에 PCM 프레임을 표시하는 채널을 하나 더 붙이고(`--pcm-channel`), 송출기가 같은 플래그로 그 채널을 읽어 떼어 냅니다. 바뀌는 지점은 프레임 단위로 정확합니다.
- **레벨·거리가 지금 설계에 없는 출력이면** (서브 수나 배치를 바꾼 뒤 남은 `SW2`, `Lb` 등) 그 항목만 알리고 빼며, 나머지 보정은 그대로 적용합니다.
- **측정이 끊기면 다시 잽니다.** 측정 신호를 보내는 동안 송출기가 100 ms 넘게 밀리면 신호에 빈틈이 생기므로, 송출기가 그 측정을 거부하고(`the sender fell behind the clock while playing; play again`) 보정 단계가 실패합니다. 그 위치를 다시 측정합니다.
- 명령줄에서는 `--write-design`(수동 설계 만들기), 송출기의 `--dsp`, `--volume`, `--control`, 디코더와 송출기 사이에 PCM 표시 채널을 두는 `--pcm-channel`(두 쪽에 함께 줍니다), 보정만 돌리는 `--dsp-only`가 있습니다(`atmos67`을 인자 없이 실행하면 사용법이 나옵니다).

### 알아 둘 것

- **Kodi가 직접 재생하는 소리.** 스테레오 음악과 메뉴 효과음처럼 Kodi가 패스스루하지 않고 PCM으로 내는 소리는 앞 스피커 두 개(FL, FR)로 나가고, 보정(측정 보정, 베이스 매니지먼트, 레벨)도 같이 받습니다. 그래서 2.0이나 2.1(`2.0` 배치에 서브 1대) 구성에서도 음악을 들을 수 있습니다. 멀티채널은 위 설정에서 Kodi가 Dolby Digital로 트랜스코딩해 보내므로 비트스트림으로 디코딩됩니다. 비트스트림이 끝나고 PCM이 시작될 때는 처음 50 ms가 빠집니다(아래 "동작").
- **PTP 하드웨어 타임스탬프.** Vero V가 지원하는지는 확인하지 못했습니다. 커널(4.9.269-93-osmc)에 `CONFIG_PTP_1588_CLOCK=y`와 이더넷 드라이버 `CONFIG_STMMAC_ETH=y`가 있고, S905X4의 Synopsys 이더넷 MAC은 보통 IEEE 1588 타임스탬프를 갖지만, OSMC 드라이버가 켜 두었는지는 실기에서 봐야 합니다. `ethtool -T eth0`에 `hardware-transmit`·`hardware-receive`와 `PTP Hardware Clock: 0` 같은 번호가 있으면 지원하고, `software-*`만 있고 `PTP Hardware Clock: none`이면 지원하지 않습니다. 어느 쪽이든 `atmos67-ptp4l` 서비스가 시작할 때 이것을 보고 고릅니다: 있으면 `ptp4l -H`로 랜카드 클록을 맞추고 `phc2sys -a -rr`가 시스템 클록으로 옮기며(Vero가 그랜드마스터면 반대로), 없으면 `ptp4l -S`가 시스템 클록을 직접 맞춥니다. 송출기는 늘 시스템 클록을 읽으므로 동작은 같고 정밀도만 다릅니다. 보통 하드웨어는 1 µs 안쪽, 소프트웨어는 수십 µs 정도이고 네트워크와 부하에 따라 달라집니다. 수신기가 소프트웨어 정밀도로 잠기는지는 수신기마다 확인해야 합니다.
- **PTP와 시스템 시각.** 기본 설정은 네트워크에서 가장 좋은 클록이 그랜드마스터가 됩니다. 다른 장비가 그랜드마스터가 되면 Vero V의 시스템 시각이 그 장비를 따라가므로, 그 장비가 실제 시각을 주지 않으면(Dante 장비 등) 시스템 시각이 틀어지고 NTP와도 충돌합니다. Vero V의 시각을 지키려면 `/etc/atmos67/ptp4l.conf`의 `priority1`을 낮춰 Vero V를 그랜드마스터로 만듭니다.
- **영상과의 동기.** Kodi는 루프백 뒤의 지연을 모르므로, 애드온이 영상이 시작될 때마다 Kodi의 오디오 오프셋을 정해 영상을 늦춥니다("영상 동기" 범주). 오프셋은 다음 값의 합입니다.
  - 송출기가 알려 주는 값: 송출 버퍼, 보정 블록(21 ms), SECS 설계의 필터 지연
  - 캡처와 디코딩 여유: 40 ms로 둔 추정값
  - 수신기 지연: 보정 마법사가 마이크로 잽니다(`atmos67 calibrate latency`). 송출기에서 청취 위치까지의 시간이라 수신기, 앰프, 공기 중 전파가 모두 들어갑니다
  - 미세 조정
- **라이선스.** atmos67은 GPL-3.0-only입니다(`LICENSE`). 바이너리에 FFmpeg(LGPL)와 SECS 2R 크레이트(AGPL-3.0-only)가 정적으로 들어 있습니다. GPLv3 13조에 따라 함께 배포할 수 있지만, 배포할 때는 두 조건도 따라야 하고 결합물 가운데 SECS 부분에는 AGPL이 적용됩니다.

### 확인한 것과 확인하지 못한 것

Vero V 실기 없이 만들었습니다.

확인한 것 (arm64 Linux 컨테이너에서, armhf 바이너리는 qemu로 실행):

- 기존 시험 전부와 송출 시험이 armhf 바이너리로 통과합니다. 송출 시험은 12채널 입력이 두 멀티캐스트 스트림으로 나가 입력과 샘플 단위로 같고, 타임스탬프가 시스템 클록의 PTP 시간이며, SAP 안내가 나오는지 봅니다.
- 바이너리는 glibc 2.30 이하의 심볼만 쓰고(bullseye는 2.31) libc 계열 외의 공유 라이브러리가 필요 없습니다.
- `.deb`가 bullseye에 설치되고, ALSA가 `hdmi_atmos67` 정의를 Kodi가 붙이는 인자와 함께 읽고, `ptp4l`이 설정으로 뜨고, 서비스 스크립트가 `ptp4l`에서 그랜드마스터를 읽어 초당 1000패킷의 스트림과 SDP를 내보냅니다.
- OSMC 저장소의 Vero V 커널 패키지(4.9.269-93)에 `snd-aloop` 모듈이 들어 있습니다.
- 보정 처리가 SECS 2R의 CamillaDSP 내보내기와 같습니다. 시험용 7.1.4 방의 37개 경로가 SECS의 Convolution 내보내기와 상대 오차 3e-6 이내로 일치합니다.
- 보정 측정의 전체 경로. 송출기의 RTP를 받아 스피커마다 지연과 게인을 준 가짜 방을 마이크로 썼을 때, SECS가 설계한 보정이 메인 11개의 도달 시간을 같은 샘플로, 레벨을 0.01 dB 이내로 맞춥니다.
- 서비스 스크립트가 수동 보정 설계를 만들어 송출기에 넘기고, 제어 소켓이 osmc 그룹에 열립니다.

확인하지 못한 것:

- Kodi가 `atmos67 (AES67)` 장치를 목록에 보여 주고 TrueHD·DTS-HD까지 패스스루하는지. OSMC의 Kodi는 Amlogic용으로 고친 빌드입니다.
- 루프백 캡처부터 디코더까지의 경로(시험 환경에 `snd-aloop`이 없습니다)와 Kodi가 포맷을 바꿀 때의 재시작.
- 실제 AES67 수신기와의 호환, Vero V 이더넷의 PTP 정확도, 하드웨어 타임스탬프 유무(위 "PTP 하드웨어 타임스탬프").
- 송출 루프가 측정이나 디코딩 부하 중에 100 ms 넘게 밀리는지. 밀리면 송출기 로그에 `the timeline was N ms off the clock`이 남습니다. 시험 VM에서는 자주 밀렸지만 VM의 네트워크 탓으로 보입니다(아래 "시험").
- **성능.** Vero V의 Cortex-A55는 아래 "성능"의 M1보다 몇 배 느리고 32비트로 돕니다. M1에서 실시간의 3~5배인 TrueHD Atmos와 E-AC-3 JOC는 실시간을 따라가지 못할 수 있습니다. 스피커 수는 부하에 거의 영향이 없습니다(아래 "스피커 수와 부하").
- 애드온은 XML과 Python 문법, Kodi에 의존하지 않는 부분의 단위 시험만 확인했고 Kodi에서 실행해 보지 않았습니다.
- 패스스루 중에도 Kodi가 볼륨 키에 `Application.OnVolumeChanged` 알림을 보내는지. 보내지 않으면 볼륨 키를 애드온에 연결하는 키맵이 필요합니다.
- USB 측정 마이크(Vero V 앞면의 USB 포트에 꽂습니다)가 Vero V에서 잡히는지, osmc 사용자가 녹음할 수 있는지.
- Kodi가 직접 재생하는 스테레오 PCM에 Kodi가 실제로 볼륨을 거는지(`--pcm-channel`의 전제), 볼륨 키로 PCM과 패스스루가 같은 크기로 들리는지.
- **측정 중 녹음 끊김.** `arecord`가 오버런을 알리면 그 측정은 실패로 처리하고 다시 재게 합니다. 녹음이 빠진 채로 쓰면 뒤쪽 스피커가 앞당겨져 보정이 틀어지기 때문입니다. 실기에서 오버런이 자주 나는지는 아직 모릅니다.
- **측정 중 클록 차이.** 마이크(USB)와 송출(PTP)은 다른 클록입니다. 한 위치의 측정이 길어지면 뒤쪽 스피커의 지연이 그 차이만큼 틀어집니다(20 ppm에 60초면 약 1.2 ms). 또 마이크 경로의 지연이 커지면(시험에서 1초 넘게 늦었을 때) SECS가 측정을 잘못 해석합니다.
- **영상 동기.** Kodi의 오디오 오프셋 부호(음수가 영상을 늦추는지)와 `Player.SetAudioDelay`가 OSMC의 Kodi에서 듣는지. 마이크로 잰 지연에는 USB 마이크 자체의 지연(보통 몇 ms)이 더해집니다.
- 보정 처리의 실기 부하. M1에서는 실시간의 6~45배였습니다(아래 "보정 DSP"). 디코딩과 다른 프로세스라 다른 코어를 씁니다.

## 동작

- 손상된 버스트는 버리고 다음 버스트부터 다시 디코딩합니다.
- 디코딩된 오디오가 없는 구간(일시정지, 미지원 포맷)은 무음으로 채워 출력이 캡처 클록을 따라가게 합니다.
- 스테레오 캡처에서 IEC 61937 동기 워드가 50 ms 넘게 없으면 PCM으로 보고 FL, FR로 냅니다. 다음 동기 워드가 나오면 그 앞까지가 PCM이고 거기서부터 비트스트림입니다. 비트스트림이 끝난 뒤의 처음 50 ms에는 마지막 버스트의 나머지가 섞여 있으므로 내보내지 않고, 그 전에 디코더가 쥐고 있던 소리를 먼저 냅니다. 16비트가 아닌 캡처(`--input-format S32_LE|FLOAT_LE`)는 처음부터 PCM입니다.
- E-AC-3는 버스트마다 OpenJOC에 먼저 주고, 거부하면 그 버스트의 채널 베드를 냅니다. 네 번 연속 거부하면 약 1초에 한 번만 다시 시도합니다.
- TrueHD 유닛 하나가 손상되면 디코더만 다음 동기 지점부터 다시 시작하고, 객체와 그 움직임은 그대로 둡니다. E-AC-3에서 OpenJOC가 버스트를 거부해 베드로 넘어갈 때도 OpenJOC가 잡고 있던 앞 유닛의 출력은 냅니다.
- 송출기는 입력(stdin)이 끝나면 쥐고 있던 것을 마저 보내고 SAP 삭제 알림을 보낸 뒤 끝납니다. 서비스를 멈추면 `atmos67-run`이 캡처를 멈춰 송출기의 입력을 끝내므로(systemd `KillMode=mixed`), 수신기 목록에 옛 스트림이 남지 않습니다.
- 송출기의 큐가 재생 중 1초 동안 버퍼보다 10 ms 넘게 많으면 초과분을 버립니다(`input ran N ms ahead; dropped it`). 실제 지연이 `latency`로 알리는 값을 넘지 않게 하려는 것이고, 그 순간 소리가 짧게 건너뜁니다.
- 송출 루프가 시계와 100 ms 넘게 어긋나면 타임라인을 시계로 다시 맞추고 `the timeline was N ms off the clock; restarted from it`를 남깁니다. 그때 측정 신호를 보내던 중이면 그 측정을 거부합니다.
- TrueHD Atmos의 객체는 OAMD의 위치와 게인을 따르고, 갱신 사이는 지정된 길이로 보간합니다. 새 메타데이터가 오면 같은 시각 이후로 잡혀 있던 옛 갱신은 버립니다. 객체 크기, 스냅, 구역 제한, 트림은 반영하지 않습니다. 갱신 블록의 시작 시점 단위(32 샘플)는 규격을 확인하지 못한 추정입니다.

## 시험

`cargo test`는 FFmpeg로 만든 합성 픽스처(채널마다 다른 주파수의 사인파)로 각 채널이 맞는 스피커로 가는지 봅니다. `cargo test --release -- --ignored`는 `samples/`의 공개 테스트 파일로 다음을 확인합니다.

| 샘플 | 결과 |
|---|---|
| AC-3 5.1, E-AC-3 5.1·7.1, TrueHD 7.1, DTS-HD MA 5.1·7.1, DTS-HD HRA 7.1 | 같은 배치로 렌더한 출력이 FFmpeg의 디코딩과 샘플 단위로 같고, 출력 길이가 캡처 길이와 같습니다. 기준 디코드는 컨테이너가 건너뛰라고 표시한 샘플도 남깁니다(`-flags2 +skip_manual`). IEC 61937에는 그 표시가 없어 atmos67은 그 샘플도 내기 때문입니다. FFmpeg 8.1은 `.dtshd` 헤더의 코덱 지연 1024 샘플을 그렇게 표시합니다. |
| DTS:X 7.1.4 | 7.1 베드가 FFmpeg의 디코딩과 같고, 객체를 렌더하지 않는다고 한 번 알립니다. |
| Atmos test tones 1000Hz V4 (OpenJOC가 거부) | E-AC-3 코어의 5.1 베드가 FFmpeg의 디코딩과 같습니다. |
| TrueHD Atmos 7.1.4 (스피커를 1분씩 호명) | 12개 스피커 모두 호명된 스피커에서만 소리가 나고(다른 스피커는 80 dB 이상 낮음), 같은 파일의 E-AC-3 JOC 트랙 렌더와 가장 큰 스피커가 12/12 일치합니다. |
| E-AC-3 JOC 5.1.2 | 첫 액세스 유닛만 OpenJOC가 거부해 베드로 나오고, 나머지는 객체로 디코딩됩니다. |

확인하지 못한 것:

- TrueHD Atmos 시험 샘플의 객체는 모두 스피커 위치에 고정돼 있습니다. 움직이는 객체와 방 안쪽에 놓인 객체는 단위 시험으로만 확인했고, 실제 콘텐츠로는 확인하지 못했습니다.
- E-AC-3 JOC에서 객체로 실린 채널은 OpenJOC가 맞는 스피커를 가장 크게 내지만 이웃 스피커에도 2~3 dB 낮게 함께 냅니다.

샘플 시험이 쓰는 파일은 `samples/Audio Test Files/`에 이 이름으로 둡니다(git에 넣지 않습니다): `Dolby TrueHD 7.1.mkv`, `DTS-HD MA 5.1.mkv`, `DTS-HD MA 7.1.mkv`, `DTS-HD High Resolution 7.1.dtshd`, `DTS-X 7.1.4.mkv`, `Dolby Digital AC-3 5.1.mp4`, `Dolby Digital Plus E-AC-3 5.1.mp4`, `Dolby Digital Plus E-AC-3 7.1.mp4`, `Atmos test tones 1000Hz V4.mp4`, `Dolby Atmos test by Buzz_Buzz_Buzz_ - 7.1.4 master with 5.1 E-AC-3 bed.mp4`, `Dolby Atmos E-AC-3 5.1.2.mp4`, `Dolby Atmos TrueHD, E-AC-3 7.1.4.mkv`(모두 1.6 GB). 없는 파일의 시험은 건너뜁니다.

송출과 보정 측정의 시험은 실시간으로 돕니다.

- `tests/send.rs`는 송출기를 띄워 멀티캐스트를 받습니다. 송출기는 시작하자마자 무음을 보내므로, 패킷을 기다리기 전에 쌓인 것을 비웁니다.
- `tests/calibrate.rs`(`--ignored`, 몇 분)는 송출기의 RTP를 가짜 방이 받아 마이크로 쓰고, SECS 2R이 설계한 보정이 그 방을 되돌리는지 봅니다. 송출기가 측정을 거부하면(루프가 밀림) 5번까지 다시 잽니다.
- **VM에서 돌릴 때.** Apple `container` machine에서는 멀티캐스트를 컨테이너 네트워크로 내보내면 송출이 수백 ms씩 멈춰 측정이 매번 거부됐습니다. 시험 바이너리를 `cargo test --release --test calibrate --no-run`으로 만든 뒤, `docker run --network none --cap-add NET_ADMIN …` 안에서 `ip link set lo multicast on; ip route add 224.0.0.0/4 dev lo`를 하고 바이너리를 `--ignored`로 직접 실행하면 멀티캐스트가 루프백에만 돕니다. 네트워크가 없으면 rustup이 툴체인을 받지 못하므로 빌드는 네트워크가 있는 컨테이너에서 합니다. VM을 오래 켜 두면 대기 시간이 크게 늘어서, 다시 시작하는 편이 낫습니다.

## 성능

Apple M1의 aarch64 Linux VM(Apple `container` machine)에서 릴리스 빌드로 실제 샘플 60초를 `--layout 7.1.4`로 처리한 시간입니다. 스레드 하나로 돌고, 세 번씩 잰 값입니다(초 단위).

| 포맷 | 처리 시간 | 실시간 대비 |
|---|---|---|
| DTS-HD MA 7.1 | 1초 이하 | 60배 이상 |
| TrueHD 7.1 | 11~12초 | 약 5배 |
| E-AC-3 JOC 7.1.4 (천장 객체 구간) | 16초 | 약 3.7배 |
| TrueHD Atmos 7.1.4 | 19초 | 약 3.2배 |

AC-3와 Atmos가 아닌 E-AC-3도 1초 안팎입니다(1회 측정).

- Vero V 실기에서 잰 값이 아닙니다. Vero V의 Cortex-A55는 M1보다 코어당 몇 배 느리고, 패키지는 32비트(armhf)로 돕니다. 그래서 M1에서 실시간의 3~5배인 TrueHD와 E-AC-3 JOC는 Vero V에서 실시간을 따라가지 못할 수 있습니다. 보정 DSP는 다른 프로세스라 다른 코어를 씁니다(아래 "보정 DSP").
- TrueHD 경로는 `truehd` 크레이트를 최적화 없이 빌드한 상태의 값입니다. 메모리가 넉넉한 머신에서 최적화해 빌드하면 더 빨라질 여지가 있습니다.
- LTO와 `codegen-units=1`은 E-AC-3 JOC 처리 시간을 바꾸지 못했습니다.

### 보정 DSP

보정은 송출기 프로세스(`--aes67-send --dsp`)에서 디코더와 다른 코어로 돕니다. 같은 VM에서 `--dsp-only`로 60초 분량의 7.1.4 입력을 처리한 시간입니다. 스레드 하나로 돌았고, 세 번 잰 값입니다.

| 설계 | 처리 시간 | 실시간 대비 |
|---|---|---|
| 수동 베이스 매니지먼트 (서브 2대, 80 Hz) | 1.3초 | 약 45배 |
| SECS 2R 시험용 방 (저지연 모드, 채널당 FIR 2048탭과 PEQ 최대 16개, 서브 3대) | 6.6초 | 약 9배 |
| 같은 설계의 FIR을 65536탭으로 늘린 것 | 10.4초 | 약 5.8배 |

실기에서는 같은 명령으로 잽니다.

```sh
head -c $((60 * 48000 * 12 * 4)) /dev/zero | time atmos67 --layout 7.1.4 --dsp /var/lib/atmos67/design --dsp-only > /dev/null
```
- GPU·NPU·영상 코덱 가속기는 쓰지 않습니다. 오디오 디코딩은 순차적인 비트스트림 해석이 대부분이라 맞지 않습니다.

### 스피커 수와 부하

같은 VM에서 60초 분량을 처리한 시간입니다(2026-11-25, 한 번씩 잼). 24·32·48채널은 시험용으로 만든 링 배치입니다.

| 출력 채널 | TrueHD Atmos 7.1.4 | E-AC-3 JOC 5.1.2 | 수동 보정 DSP |
|---|---|---|---|
| 2 (2.0) | 15.4초 | 10.7초 | 0.05초 |
| 12 (7.1.4) | 15.6초 | 11.5초 | 0.23초 |
| 16 (9.1.6) | 15.4초 | 12.0초 | 0.28초 |
| 32 | 15.6초 | 13.6초 | |
| 48 | 15.9초 | 15.1초 | |

- 시간 대부분은 디코딩에 듭니다. 디코딩 시간은 스피커 수와 상관이 없습니다. 스피커 수에 비례하는 것은 렌더와 보정입니다. E-AC-3 JOC는 2채널에서 48채널로 늘려도 40 % 느려질 뿐이고, TrueHD Atmos는 거의 그대로입니다.
- SECS 2R 보정은 채널 수와 FIR 길이에 비례합니다. 위 "보정 DSP"의 12채널 2048탭이 6.6초이므로 채널당 약 0.55초입니다.
- 송출량은 8채널 스트림 하나가 초당 1000패킷, 약 10 Mbit/s입니다. 48채널이어도 6개 스트림 60 Mbit/s라 기가비트 이더넷에는 여유가 큽니다.

그래서 Vero V에서 스피커를 몇 대까지 쓸 수 있는지는 채널 수보다 포맷이 정합니다. 걱정할 것은 TrueHD Atmos와 E-AC-3 JOC의 디코딩이 한 코어에서 실시간을 따라가는지입니다. 이것이 모자라면 스피커를 줄여도 풀리지 않습니다. 실기에서 잴 때까지는 알 수 없습니다.

## 제약

- **HDCP**: 보호된 신호는 다루지 않습니다. HDCP 키가 없는 수신기에는 보호 소스가 신호를 보내지 않으며, 이 프로그램은 캡처 장치가 받아 ALSA로 내주는 오디오만 처리합니다.
- **HDMI 입력 하드웨어**: 보드에 HDMI 입력 포트와 그 ALSA 캡처 드라이버가 있어야 합니다. Orange Pi 5 계열은 모델마다 HDMI 입력 유무가 다르므로 확인이 필요합니다.
- **비트 단위 캡처**: HDMI 입력 드라이버가 캡처 데이터를 변형 없이(리샘플·볼륨·믹싱 없이) 내줘야 비트스트림이 살아남습니다. 실기에서 확인하지 않았습니다.
- **EDID**: 소스가 원하는 포맷을 보내려면 HDMI 입력의 EDID가 그 포맷 지원을 알려야 합니다.
