# OSMC (Vero V) 패키징 — 진행 상황

2026-11-13 기준. 사용법은 README의 "OSMC (Vero V) 패키지" 절에 있고, 이 문서는 이어서 작업할 때 필요한 결정·사실·남은 일을 적는다.

> **2026-11-25: 패키징을 `kodi-extras`로 옮겼다.** 아래의 `packaging/…`은 이제 `kodi-extras`의 `packages/atmos67/…`(deb 트리, `Dockerfile`, `build.sh`, `Dockerfile.aarch64`, `build-aarch64.sh`)와 `addons/service.atmos67/`(애드온), `tests/test_atmos67.py`(애드온 시험)다. 패키지는 이 저장소의 커밋(`packages/atmos67/atmos67-source`의 `ATMOS67_REV`)을 꺼내 빌드한다.

## 상태

- `.deb`와 Kodi 애드온 zip이 빌드되고 컨테이너 시험을 통과했다. **Vero V 실기에서는 아무것도 돌려 보지 않았다.**
- **커밋하지 않았다.** 작업 트리에 남아 있는 것:
  - 수정: `README.md`, `src/main.rs`, `src/aes67.rs`(`CHANNELS_PER_STREAM`을 `pub`으로)
  - 추가: `src/send.rs`, `tests/send.rs`, `packaging/`, `tasks/vero-v.md`
- `SPEC.md`는 고치지 않았다. 범위가 달라졌으므로(사용자 공간 송출기, Vero V) 커밋 전에 갱신할지 정해야 한다.

## 결정한 것

| 항목 | 결정 | 이유 |
|---|---|---|
| 오디오 입력 | Kodi 패스스루 → `snd-aloop` 루프백 → `arecord` | Vero V에 HDMI 입력이 없다 |
| AES67 송출 | `atmos67 --aes67-send` (사용자 공간 RTP + SAP) | 커널이 `CONFIG_MODULE_SIG_FORCE=y`라 RAVENNA 모듈을 올릴 수 없다. 서명 키는 배포되지 않는다 |
| PTP | linuxptp. 송출기는 시스템 클록에서 PTP 시간을 읽는다(UTC + 37초) | 새 의존성과 `unsafe` 없이 가능하다. 루프백도 시스템 클록으로 돌아 입력과 송출 속도가 같으므로 리샘플이 필요 없다 |
| 배포 형태 | `.deb`(바이너리·서비스) + Kodi 서비스 애드온 zip(설정 UI) | |
| FFmpeg | 8.0을 AC-3·E-AC-3·DTS 디코더만 넣어 정적 링크 | bullseye의 FFmpeg는 4.3이라 시스템 것에 기댈 수 없다 |

## 조사로 확인한 사실

OSMC apt 저장소(`http://apt.osmc.tv`, `bullseye`)에서 확인했다.

- userland는 Debian bullseye **armhf**, 커널은 arm64 `4.9.269-93-osmc`(패키지 `vero564-image-*`), Kodi는 21.3(`vero5-mediacenter-osmc 21.3.0-7`).
- 커널 패키지에 `snd-aloop.ko`가 있다(`CONFIG_SND_ALOOP=m`).
- `CONFIG_MODULE_SIG_FORCE=y`, `CONFIG_PTP_1588_CLOCK=y`, `CONFIG_STMMAC_ETH=y`, `CONFIG_HZ=100`.
- bullseye-security는 미러에서 빠졌다. Dockerfile은 그 소스를 지우고 빌드한다. 실기의 `apt install`이 `linuxptp` 등을 받을 수 있는지는 확인하지 못했다.

## 구성

```
Kodi ─> hdmi_atmos67 (루프백 재생) ─> arecord (hw:Loopback,1,0) ─> atmos67 ─> atmos67 --aes67-send
```

- `packaging/deb/usr/lib/atmos67/atmos67-run`: 서비스 본체. `/proc/asound/Loopback/pcm0p/sub0/hw_params`를 0.2초마다 보고 Kodi가 연 형식(2/8채널, 레이트)으로 캡처와 디코더를 띄우며, 형식이 바뀌면 `arecord`를 죽이고 다시 띄운다. 송출기는 계속 살아 있고 입력이 없으면 무음을 보낸다. 그랜드마스터가 바뀌면 스스로 끝나 systemd가 다시 시작한다(SDP에 그랜드마스터가 적히므로).
- `packaging/deb/usr/lib/atmos67/atmos67-ptp`: `ethtool -T`에 하드웨어 타임스탬프가 있으면 `ptp4l -H` + `phc2sys -a -rr`, 없으면 `ptp4l -S`.
- `packaging/deb/etc/alsa/conf.d/50-atmos67.conf`: Kodi는 이름이 `hdmi`로 시작하는 장치에만 모든 포맷의 패스스루를 허용하므로 이름이 `hdmi_atmos67`이다.
- `packaging/deb/etc/modprobe.d/atmos67.conf`: `pcm_notify=1`. 캡처가 열려 있어도 Kodi가 다른 형식으로 다시 열 수 있게 한다.
- 설정: `/etc/atmos67/atmos67.conf`가 기본값, 애드온이 쓰는 `/home/osmc/.kodi/userdata/addon_data/service.atmos67/atmos67.conf`가 우선한다(둘 다 systemd `EnvironmentFile`).
- 애드온(`packaging/kodi/service.atmos67/`): `service.py`가 설정을 파일로 쓰고 `sudo -n systemctl restart`를 한다. `audio.py`는 JSON-RPC로 Kodi 오디오 설정을 바꾼다.

## 빌드 (이 Mac)

`target/` 아래의 래퍼는 git에 없다(`.gitignore`).

```sh
# VM 재부팅 후 한 번: 32비트 ARM용 qemu 등록 (시험이 armhf 바이너리를 실행한다)
container machine run --root -n linux-test -- sh /Users/saya6k/Projects/atmos67/target/binfmt.sh
# 이미지 빌드, 시험, 패키지 조립, 설치 시험. 결과는 target/packages/
container machine run --root -n linux-test -- sh /Users/saya6k/Projects/atmos67/target/pkg.sh
```

- `target/armhf.sh [스크립트]`: 교차 빌드 컨테이너에서 스크립트를 돌린다. `target/clippy-inner.sh`가 clippy.
- `target/pkgtest-inner.sh`: 컨테이너에 `.deb`를 설치하고 ALSA 설정, `ptp4l`, 서비스 스크립트의 송출 경로를 본다.

빌드하며 만난 문제:

- ffmpeg-sys-next 9.1.1의 Vulkan 바인딩 스텁이 32비트에서 컴파일되지 않는다. FFmpeg 설치 경로에서 `hwcontext_vulkan.h`를 지워 그 바인딩이 빠지게 했다.
- 바인드 마운트 위에서는 `sed -i`가 실패해 `build.sh`가 `mktemp -d`에서 조립한다.

## 확인한 것

- armhf 바이너리로 단위 시험 34개, `tests/e2e.rs` 11개, `tests/send.rs` 1개 통과. clippy 경고 없음. `tests/samples.rs` 12개(`--ignored`)는 armhf로 돌리지 않았다.
- 바이너리는 glibc 2.30 이하 심볼만 쓰고 libc 계열 외 공유 라이브러리가 없다.
- `.deb` 설치, `hdmi_atmos67:AES0=…` 형식의 이름 해석, `ptp4l` 설정, 그랜드마스터 읽기, 초당 1000패킷 송출과 SDP.

2026-11-19 (리뷰 수정 뒤, `tasks/review.md`), arm64 네이티브로:

- 단위 71, `tests/e2e.rs` 12, `tests/send.rs` 5, `tests/samples.rs` 12(`--ignored`), 애드온 파이썬 15개 통과. clippy 경고 없음.
- `tests/calibrate.rs`는 멀티캐스트를 루프백에 둔 컨테이너에서 통과했다(README "시험"). 컨테이너 네트워크로 내보내면 VM 쪽에서 송출이 멈춰 측정이 거부된다.
- armv7(패키지 대상)로도 확인했다(2026-11-19, 스테레오 PCM 지원까지 넣은 `ad0dc9a`): libswresample을 넣어 이미지를 다시 빌드했고, armhf 바이너리로 단위 78, `tests/e2e.rs` 15, `tests/send.rs` 5 통과. `.deb`(6.7 MB) 설치, 공유 라이브러리는 libc 계열뿐(GLIBC 2.30 이하), 서비스 스크립트가 FIFO로 송출기를 띄워 3초에 RTP 3001패킷과 SDP를 내고, 수동 보정 13채널과 osmc 그룹 제어 소켓이 동작했다. `tests/samples.rs`와 캘리브레이션 E2E는 armhf로 돌리지 않았다.

2026-11-25 (`--pcm-channel` 등 `tasks/review.md`의 수정 뒤): armhf로 단위 80, `tests/e2e.rs` 16, `tests/send.rs` 6 통과, `.deb` 설치 시험 통과. arm64 네이티브 릴리스로 `tests/samples.rs` 12, `tests/calibrate.rs`, `secs_parity` 통과.

## 남은 일

실기가 있어야 하는 것부터.

1. **설치와 장치 확인.** Vero V에서(측정 마이크는 앞면 USB 포트):
   ```sh
   uname -r; dpkg --print-architecture
   sudo apt install ./atmos67_0.1.0_armhf.deb
   lsmod | grep snd_aloop; cat /sys/module/snd_aloop/parameters/pcm_notify
   aplay -L | grep -A1 atmos67
   ethtool -T eth0
   systemctl status atmos67 atmos67-ptp4l atmos67-phc2sys
   journalctl -u atmos67 -u atmos67-ptp4l -b
   ```
   커널이 4.9.269-93이 아니면 `snd-aloop`과 모듈 서명 설정을 다시 확인한다.

   `ethtool -T eth0`으로 PTP 하드웨어 타임스탬프를 확인한다(2026-11-19, 실기가 없어 아직 모른다). `hardware-transmit`·`hardware-receive`가 있고 `PTP Hardware Clock`이 번호면 지원한다. 그러면 `atmos67-ptp4l`이 `ptp4l -H`로, `atmos67-phc2sys`가 `phc2sys -a -rr`로 돌아야 한다(`systemctl status`의 명령줄). `software-*`만 있고 `PTP Hardware Clock: none`이면 `ptp4l -S`다. 근거: 커널에 `CONFIG_PTP_1588_CLOCK=y`, `CONFIG_STMMAC_ETH=y`가 있고, S905X4의 Synopsys 이더넷 MAC은 보통 IEEE 1588 타임스탬프를 갖는다(일반 지식, 이 칩에서 확인하지 않음). 정밀도는 보통 하드웨어 1 µs 안쪽, 소프트웨어 수십 µs이며 수신기가 잠기는지가 실제 기준이다.

   송출기 로그에서 `the timeline was N ms off the clock`(루프가 100 ms 넘게 밀림)과 `input ran N ms ahead`(입력이 버퍼보다 앞서 버림)가 얼마나 나오는지도 본다. 앞의 것이 측정 중에 나면 그 측정은 거부된다.
2. **Kodi.** 애드온 설치 → 장치 목록에 `atmos67 (AES67)`이 보이는지 → "Kodi 오디오 출력을 atmos67로 설정"이 통하는지 → 포맷별 재생 중 `cat /proc/asound/Loopback/pcm0p/sub0/hw_params`와 `journalctl -u atmos67 -f`. 장치가 안 보이거나 HD 포맷 패스스루가 안 되면 OSMC의 ALSA 싱크 패치를 살펴야 한다. 스테레오 PCM에 Kodi가 볼륨을 거는지(`--pcm-channel`의 전제)와, 볼륨 키로 PCM과 패스스루가 같은 크기로 들리는지도 본다.
3. **성능.** 가장 큰 위험이다. 실기에서 `atmos67`에 파일을 넣어 실시간 대비 속도를 잰다(README "성능"의 방식, `target/bench.sh` 참고). `truehd`는 2026-11-25부터 최적화해 빌드한다(rustc 1.100에서 메모리 약 1.3 GB, VM에서 TrueHD Atmos가 약 4배 빨라짐). 가장 느린 경로는 E-AC-3 JOC(M1 VM에서 실시간의 4배 안팎)다. 32비트 패키지 바이너리와 정적 aarch64 바이너리(`packaging/build-aarch64.sh`, README "64비트 바이너리 (시험용)")를 같은 입력(`target/packages-aarch64/bench/`)으로 재서 비교한다. aarch64 바이너리는 VM의 armhf bullseye 컨테이너에서 도는 것까지 확인했다(2026-11-25). 스피커 수는 부하에 거의 영향이 없다(2026-11-25 VM에서 2→48채널에 TrueHD Atmos는 그대로, E-AC-3 JOC는 40 % 증가, README "스피커 수와 부하").
4. **포맷 전환.** 캡처 재시작이 실제로 되는지, `arecord`가 깔끔히 끝나는지.
5. **수신기.** 실제 AES67 수신기가 SAP를 보고 잠기는지, PTP 역할(그랜드마스터/슬레이브)과 시스템 시각·NTP 충돌.
6. **지연.** 버퍼 100 ms가 끊김 없이 충분한지, Kodi 오디오 오프셋 값.

실기 없이 할 수 있는 것:

- ~~`SPEC.md` 갱신 여부 결정 후 커밋.~~ SPEC 머리에 범위 변경을 적었다.
- ~~미정: PCM(스테레오 음악, 메뉴 효과음)이 무음인 것을 그대로 둘지.~~ 2026-11-19: 스테레오 PCM을 FL, FR 베드로 내고 보정을 함께 적용하기로 했다(2.0·2.1 구성 때문). 리샘플은 atmos67이 FFmpeg로, 멀티채널은 지금처럼 Dolby Digital 트랜스코딩. `atmos67-run`은 `S16_LE`(2·8채널), `S32_LE`·`FLOAT_LE`(2채널)를 캡처한다. 실기에서 볼 것: Kodi가 PCM을 어떤 형식·레이트로 여는지(`cat /proc/asound/Loopback/pcm0p/sub0/hw_params`), 음악과 메뉴 효과음이 나는지, 패스스루와 PCM을 오갈 때 끊김이나 잡음이 없는지. armhf 이미지는 libswresample을 넣어 다시 빌드했다(위 "확인한 것").
- ~~미정: CamillaDSP(룸 보정)를 이 경로에 넣을지.~~ 넣지 않고 송출기가 SECS 2R 엔진으로 보정한다(`tasks/dsp.md`).
- ~~미정: 라이선스.~~ GPL-3.0-only(`LICENSE`, README "라이선스").
