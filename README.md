# Codex Rate Proxy

Codex CLI와 OpenAI 호환 Responses API를 위한 소형 로컬 요청 속도 제한 프록시입니다.
주 구현은 CentOS 7.4 같은 구형 Linux에서도 실행할 수 있는 Rust 정적 바이너리이며,
대안으로 Python 표준 라이브러리만 사용하는 구현도 포함합니다.

Codex가 짧은 시간에 여러 모델 요청을 보내 API 서버에서 HTTP
`429 Too Many Requests` 오류가 발생할 때 유용합니다.

## 주요 기능

- API 서버로 보내는 요청의 시작 시각 사이에 최소 간격 적용
- HTTP 429 응답에 지수 백오프 적용
- API 서버의 `Retry-After` 헤더 반영
- SSE 응답의 페이로드를 변경하지 않고 스트리밍 전달
- INI 설정 파일에서 실행 옵션 읽기
- 기본적으로 `127.0.0.1`에서만 연결 수신
- `x86_64-unknown-linux-musl` 정적 바이너리 배포
- 실행 시 Python, OpenSSL, libcurl 설치나 root 권한 불필요
- 명령 하나로 Codex 실행, API 키별 프록시 재사용 및 자동 포트 할당
- 사용하지 않는 프록시 정리 시 활성 Codex 세션과 스트리밍 요청 보호
- 사용자 이름으로 API 키를 등록하여 재사용

## Codex 실행 (Linux, Bash 및 csh/tcsh)

`launch`, `url`, `stop`에서 **`-u`**는 `--user`와 동일합니다.

```sh
codex-rate-proxy launch -u hoony
codex-rate-proxy launch -u hoony -- resume --last
codex-rate-proxy url -u hoony
codex-rate-proxy stop -u hoony
```

기존 `--user`도 계속 지원합니다. 두 표기 중 하나만 한 번 사용해야 하며,
`-u`와 `--user`를 함께 쓰거나 다른 키 입력 옵션과 함께 쓰면 오류가 발생합니다.

Rust 구현의 `launch` 명령은 프록시를 준비하거나 기존 프록시를 재사용한 뒤 Codex를 실행합니다.
공통 INI 파일은 `~/.config/codex-rate-proxy/config.ini` 하나를 사용하세요.
기존 사용자 지정 provider(기본값 `corp`)와 모델은 `~/.codex/config.toml`에 정의합니다.
공통 INI에는 사용자별 포트나 키 파일 경로를 넣지 않습니다.

```sh
# 최초 한 번 등록합니다. 키는 화면에 표시되지 않으며 이후에는 다시 입력하지 않습니다.
codex-rate-proxy register hoony
codex-rate-proxy launch --user hoony
codex-rate-proxy launch --user hoony -- resume --last

# 현재 셸의 OPENAI_API_KEY를 사용합니다. 없으면 화면에 표시하지 않고 입력받습니다.
codex-rate-proxy launch

# 키 입력 방법을 명시합니다. 하나만 선택하세요. 키 파일은 한 줄로 작성합니다.
codex-rate-proxy launch --key-file /path/to/my-key
codex-rate-proxy launch --key-env MY_LLM_KEY
codex-rate-proxy launch --ask-key
key-producing-command | codex-rate-proxy launch --key-stdin

# -- 뒤에 Codex에 전달할 인자를 지정합니다.
codex-rate-proxy launch -- --model YOUR_MODEL

# Codex 실행 없이 프록시만 준비하거나 재사용하고 URL을 출력합니다.
codex-rate-proxy url --key-file /path/to/my-key
```

위 명령은 Bash와 csh/tcsh에서 동일하게 사용합니다. 관리 모드에서는 `nohup`이나
PID 파일을 다루는 명령이 필요하지 않습니다. `launch`는 로컬 URL을 표준 오류(stderr)에,
`url`은 URL만 표준 출력(stdout)에 출력합니다. `--key-stdin` 사용 시 입력은
EOF로 끝나야 합니다. 터미널이 있으면 Codex의 표준 입력을 `/dev/tty`에 다시 연결합니다.

### 사용자 등록

`register NAME`은 명시적으로 실행할 때만 키를 저장합니다.
`OPENAI_API_KEY`가 있어도 기본적으로 키를 화면에 표시하지 않고 직접 입력받습니다.
대화형 입력 없이 등록하려면 `--key-file PATH`, `--key-env NAME`,
`--key-stdin` 중 하나를 선택하세요.

```sh
codex-rate-proxy register hoony --key-file /path/to/my-key
codex-rate-proxy register hoony --replace --key-env MY_LLM_KEY
codex-rate-proxy url --user hoony
codex-rate-proxy stop --user hoony
```

이름은 ASCII 영문자, 숫자, 밑줄 또는 하이픈으로 구성된 1~64자여야 합니다.
기존 등록 정보를 변경하려면 `--replace`가 필요합니다.
키는 `~/.config/codex-rate-proxy/users/NAME.key`에 암호화하여 저장하며,
파일 권한은 `600`, users 디렉터리 권한은 `700`입니다.
공통 INI에는 개인 경로를 저장하지 않습니다.

`--user`는 저장된 키를 선택하며 다른 키 입력 옵션과 함께 사용할 수 없습니다.
Linux 계정 전체에 적용되는 기본 사용자는 없으므로 각자가 자신의 이름을 선택합니다.
`--user`를 생략하면 기존처럼 환경변수 또는 직접 입력을 사용합니다.
사용자 등록은 편의 기능이며, 같은 Linux UID를 공유하는 사람 사이의 격리를 제공하지 않습니다.
두 이름의 키와 설정이 같으면 동일한 프록시를 재사용합니다.
저장된 키를 교체하면 이후 실행부터 적용되며, 기존 세션은 기존 키를 유지합니다.

자격 증명은 [RustCrypto XChaCha20-Poly1305](https://docs.rs/chacha20poly1305/0.10.1/chacha20poly1305/)로
암호화합니다. 저장할 때마다 새로운 무작위 nonce를 사용하며, 인증 데이터는 사용자 이름에 연결됩니다.
파일은 버전 정보가 있는 바이너리 형식이므로 내용을 수정하거나 이름을 바꾸면 복호화에 실패합니다.
무작위 256비트 마스터 키는 로컬의
`~/.local/share/codex-rate-proxy/master.key`에 생성합니다
(파일 권한 `600`, 디렉터리 권한 `700`).
추가 암호, 환경변수, INI 옵션, 키링 서비스 또는 실행 환경의 패키지 설치가 필요하지 않습니다.
암호화 코드는 바이너리에 포함됩니다.

이 방식은 자격 증명 파일만 단독으로 유출된 경우를 보호합니다.
자격 증명 파일과 마스터 키를 모두 읽을 수 있으면 복호화할 수 있습니다.
같은 Linux UID를 공유하는 사람이나 두 경로를 모두 포함한 백업도 여기에 해당합니다.
계정 탈취나 실행 중인 프로세스 내부 조회까지 보호하지는 않습니다.
등록 정보를 복원해야 한다면 마스터 키를 안전하게 백업하세요.
마스터 키를 잃으면 API 키를 다시 등록해야 합니다.
암호화된 등록 정보가 남아 있는 상태에서는 누락된 마스터 키를 자동으로 새로 만들지 않습니다.

v0.4.0의 기존 평문 등록 정보는 처음 사용할 때 자동으로 암호화합니다.
업그레이드 직후 저장된 등록 정보 **전체**를 변환하려면 다음을 실행하세요.

```sh
codex-rate-proxy encrypt-keys
```

이 명령은 INI나 API 연결 없이 실행할 수 있으며, 기존 암호화 파일도 검증합니다.
여러 번 실행해도 됩니다. 잘못된 파일을 만나면 중단하지만 이미 변환한 파일은 암호화 상태를 유지합니다.
각 파일은 평문 백업을 남기지 않고 원자적으로 교체합니다.
이전 백업이나 파일시스템 스냅샷은 삭제하지 않으며, 과거에 저장된 평문의 파일시스템 복구를 막지는 않습니다.
이전 바이너리는 암호화된 형식을 읽지 못하므로 등록 사용자 관련 명령은 모두 새 바이너리로 실행하세요.

저장된 사용자 하나를 삭제하려면 다음을 실행합니다.

```sh
codex-rate-proxy unregister hoony
```

`unregister`는 API 키나 INI가 필요하지 않으며, 자격 증명이나 마스터 키가 손상되어도 실행할 수 있습니다.
이미 없는 이름에 실행해도 문제가 없습니다. 해당 이름의 자격 증명 파일만 삭제하며,
마스터 키와 다른 등록 정보는 유지합니다. API 제공자 측에서 키를 폐기하지 않으며,
기존 세션이나 프록시도 종료하지 않습니다.
사용하지 않는 프록시까지 종료하려면 등록 해제 전에 `stop --user hoony`를 실행하거나,
해제 후 `prune`을 사용하세요. 파일 삭제는 안전한 완전 삭제를 의미하지 않습니다.

런처의 `--` 뒤에 지정한 인자는 하위 명령이나 리터럴 인자용 추가 `--`까지
그대로 Codex에 전달합니다. 런처의 설정 재정의 인자는 그 앞에 배치하므로,
Codex가 이를 프롬프트 텍스트가 아닌 옵션으로 인식합니다.
사용자가 명시적으로 전달한 Codex 설정 재정의 옵션은 Codex에서 처리합니다.

런처는 로컬 `base_url`, provider 선택, 전용 `CODEX_RATE_PROXY_API_KEY` 환경변수,
재시도 및 전송 방식 재정의 옵션을 Codex 자식 프로세스에 전달합니다.
공통 Codex 설정 파일이나 부모 셸의 환경변수는 수정하지 않습니다.
자식 프로세스의 기존 `NO_PROXY` 항목은 유지하고 루프백 주소를 추가합니다.
Rust 프록시가 API 서버에 연결할 때 사용하는 포워드 프록시 설정은
INI에서만 읽으며, 외부 환경의 HTTP_PROXY는 무시합니다.

| 상황 | 처리 |
| --- | --- |
| 키, API 서버, 정규화된 INI 절대 경로가 같음 | 실행 중인 프록시 재사용 |
| 같은 식별 정보로 동시에 실행 | 프록시 하나만 생성 |
| 키, API 서버 또는 정규화된 INI 절대 경로가 다름 | 독립된 프록시 생성 |
| 오래된 기록만 남아 있고 데몬의 생존 잠금이 없음 | OS가 할당한 포트로 다시 생성 |
| 응답하지 않는 프로세스가 여전히 잠금을 보유함 | 오류 보고; 임의 종료나 중복 생성 안 함 |
| Codex가 정상 종료함 | 재사용을 위해 유휴 제한 시간까지 유지 |
| Codex가 오류로 종료하거나 시작하지 못함 | 사용 중인 세션·요청이 없으면 이번에 새로 생성한 프록시 종료 |
| 실패한 실행에서 기존 프록시를 재사용했음 | 기존 프록시 유지 |

관리 모드의 HTTP 리스너는 `/health`를 포함하여 일치하는 Bearer 키를 요구합니다.
해당 프록시를 사용하는 모든 세션은 요청 간격과 429 대기 시간을 공유합니다.
새로운 429가 발생해도 이미 전송한 요청은 회수할 수 없습니다.
이 기능은 요청 전송 간격을 제어하며, 토큰 수를 계산하거나 TPM을 정확히 제한하지는 않습니다.
Linux 계정/HOME 디렉터리 또는 INI 경로가 다르면 속도 제한 상태를 공유하지 않습니다.

### 프록시 정리

```sh
codex-rate-proxy list
codex-rate-proxy stop --key-file /path/to/my-key
codex-rate-proxy prune --dry-run
codex-rate-proxy prune
```

`stop`은 `launch`와 동일한 키 입력 옵션 및 `--config` 옵션을 받으며,
활성 세션이나 요청이 있는 인스턴스의 종료는 거부합니다.
`list`와 `prune`은 현재 HOME 아래에서 관리하는 모든 키의 인스턴스를 대상으로 합니다.
`prune`은 사용하지 않는 인스턴스를 즉시 종료하며, `--dry-run`은 대상만 표시합니다.
응답하지 않는 인스턴스는 생존 잠금으로 종료를 확인할 수 없는 한 유지합니다.
기존 독립 실행 프록시나 관련 없는 프로세스는 대상에 포함하지 않습니다.

자동 정리는 연결된 세션이 없고, 활성 요청·스트림·대기 중인 재시도도 없는 상태가
`idle_timeout_seconds`(기본값 1800초) 동안 지속되면 실행합니다.
검사는 약 2초마다 수행합니다. Codex가 사용자 입력을 기다리거나 도구를 실행하는 동안에도
활성 상태로 취급합니다. 커널이 관리하는 사용권(lease)을 Codex가 상속하므로,
런처만 종료했다고 살아 있는 자식 프로세스를 미사용 상태로 판단하지 않습니다.
사용권을 유지한 후손 프로세스가 있으면 그 프로세스가 종료할 때까지 프록시도 유지합니다.

상태 정보는 접근 권한을 제한한 `~/.local/state/codex-rate-proxy/`에 저장합니다.
각 인스턴스는 해시 기반 식별자, 비공개 제어 토큰, 로컬 URL, PID, 로그를 가지며,
API 키나 개인 키 파일 경로는 이곳에 저장하지 않습니다.
정상 종료 시 기록, 제어 소켓, 만료된 사용권을 제거합니다.
잠금 파일 교체에 따른 경쟁 상태를 피하기 위해 작은 잠금 파일과 마지막 로그는 의도적으로 남깁니다.
같은 인스턴스를 다시 생성하면 로그를 교체합니다.
실행 상태를 저장하는 파일시스템은 Linux `flock`을 정상 지원해야 합니다.
HOME 경로는 접미사를 포함해 Unix 소켓 경로 길이 제한인 108바이트 안에 들어가야 합니다.

관리 데몬은 상태 디렉터리를 작업 디렉터리로 사용하며,
Codex는 실행을 시작한 디렉터리를 그대로 사용합니다.
같은 공통 INI를 사용하면 프로젝트 폴더만 바꿔도 프록시가 추가로 생성되지 않습니다.
INI 복사본의 정규화된 절대 경로가 다르면 의도적으로 서로 다른 인스턴스로 취급합니다.
프로젝트 간 속도 제한 상태를 공유하려면 기본 공통 INI를 사용하세요.
빈 상태 디렉터리나 잠금 파일은 실행 중인 프로세스나 좀비 프로세스가 아닙니다.
`list`로 관리 인스턴스를 확인하고 `prune`으로 사용하지 않는 인스턴스를 종료하세요.

수정 전 v0.3.0을 실제 Codex 0.153.2로 테스트했을 때, 잘못된 `config.toml`로 종료하면
유휴 데몬이 살아 있었습니다(프로세스 상태는 좀비 `Z`가 아닌 `S`).
공통 INI 하나를 사용하면 폴더를 바꿔도 재사용했고, INI 경로가 다르면 별도 데몬을 생성했습니다.
재현 코드는 커밋 `797c6ac`에 보존되어 있습니다.
현재 테스트는 잘못된 TOML 및 유효하지 않은 provider 설정으로 종료했을 때 정리되는지 검증합니다.

동일한 Linux UID를 사용하는 사람 사이에서 공유 계정의 프로세스와 파일은 보안 경계가 되지 않습니다.
API 키 인증은 서로 다른 키의 실수로 인한 혼용을 막지만 같은 UID의 사용자를 격리하지는 않습니다.
키는 파이프로 데몬에 전달하고 자식 프로세스의 환경변수로 Codex에 전달하며,
CLI 인자로 전달하지 않습니다.

### 공통 런처 정책

```ini
[launcher]
codex_binary = codex
provider = corp

[lifecycle]
idle_timeout_seconds = 1800
```

INI에는 `key_file`, 개인 경로 또는 선호하는 키 입력 방법을 저장하지 않습니다.
명시적인 키 입력 옵션은 기본 환경변수 조회보다 우선합니다.
여러 키 입력 옵션을 함께 지정하면 오류가 발생합니다.
비어 있거나 여러 줄이거나 형식이 잘못된 키는 프록시 생성 전에 거부합니다.
관리 모드의 프록시는 항상 루프백 주소의 자동 할당 포트에서 수신합니다.
`[server] host/port`는 독립 실행 모드에만 적용됩니다.

SIGHUP으로 유효한 정책 변경을 다시 읽으면 새 요청부터 적용하며,
기존 요청은 원래 정책을 유지합니다.
관리 프록시의 API 서버 주소는 다시 읽기로 변경할 수 없습니다.
주소를 변경하려면 다시 실행하여 새로운 식별 정보의 인스턴스를 생성하세요.
Python 대체 구현은 Rust의 런처 및 수명 관리 기능을 제공하지 않습니다.

## 빌드된 바이너리 다운로드

최신 GitHub Actions 실행 결과 또는 버전 태그가 있는 GitHub Release에서
`codex-rate-proxy-x86_64-linux-musl.tar.gz`를 다운로드하세요.
musl 바이너리는 정적으로 링크되어 있으며,
Rust나 Python을 설치하지 않은 x86_64 CentOS 7.4에서 실행하는 것을 목표로 합니다.

```bash
tar -xzf codex-rate-proxy-x86_64-linux-musl.tar.gz
./install.sh
```

실행 파일과 초기 설정 파일은 다음 위치에 설치됩니다.

```text
~/.local/bin/codex-rate-proxy
~/.config/codex-rate-proxy/config.ini
```

`install.sh`는 기존 `config.ini`를 보존합니다. root 권한은 필요하지 않습니다.
배포 압축파일에는 안전한 예제 설정도 `config.ini`라는 이름으로 포함합니다.

## 소스에서 빌드

현재 Linux 호스트용 GNU 빌드:

```bash
cargo test
cargo build --release
```

정적 musl 빌드:

```bash
rustup target add x86_64-unknown-linux-musl
cargo test
cargo build --release --target x86_64-unknown-linux-musl
```

빌드 결과 경로:

```text
target/x86_64-unknown-linux-musl/release/codex-rate-proxy
```

릴리스를 배포하려면 버전 태그를 푸시합니다.

```bash
git tag v0.1.0
git push origin v0.1.0
```

GitHub 워크플로가 정적 바이너리를 빌드·검증·패키징하고 체크섬을 생성한 뒤 릴리스에 첨부합니다.

## Python 대체 구현

Codex CLI를 실행하는 머신에 `llm_rate_proxy.py`를 복사합니다.

```bash
chmod 700 llm_rate_proxy.py
```

## 설정

예제 설정을 복사한 뒤 복사본을 편집합니다.

```bash
mkdir -p ~/.config/codex-rate-proxy
cp llm_rate_proxy.ini.example ~/.config/codex-rate-proxy/config.ini
chmod 600 ~/.config/codex-rate-proxy/config.ini
vi ~/.config/codex-rate-proxy/config.ini
```

```ini
[server]
host = 127.0.0.1
port = 8765

[upstream]
base_url = http://llm.example.com/v1
timeout_seconds = 600
max_request_body_bytes = 134217728

[rate_limit]
min_interval_seconds = 10
max_retries = 5
backoff_base_seconds = 5
backoff_max_seconds = 60
backoff_jitter_seconds = 1

[forward_proxy]
http = http://proxy.example.com:8080
```

API 서버에 직접 연결할 수 있다면 `http` 값을 비워 두세요.
실제 `config.ini`에는 내부 주소나 프록시 자격 증명이 포함될 수 있으므로 커밋하지 마세요.

## 실행

```bash
~/.local/bin/codex-rate-proxy
```

기본 설정 경로는 `~/.config/codex-rate-proxy/config.ini`입니다.
다른 파일을 사용하려면 다음과 같이 지정합니다.

```bash
~/.local/bin/codex-rate-proxy --config /path/to/proxy.ini
```

Python 대체 구현을 실행하려면 다음을 사용합니다.

```bash
python3 llm_rate_proxy.py --config /path/to/proxy.ini
```

Bash에서 로그아웃 후에도 실행을 유지하려면 다음을 사용합니다.

```bash
nohup ~/.local/bin/codex-rate-proxy > "$HOME/.config/codex-rate-proxy/proxy.log" 2>&1 &
echo $! > "$HOME/.config/codex-rate-proxy/proxy.pid"
```

csh 또는 tcsh에서는 `>>&`로 표준 출력과 표준 오류를 함께 이어 씁니다.
`>!`는 `noclobber`가 활성화되어 있어도 기존 PID 파일을 덮어씁니다.

```csh
nohup ~/.local/bin/codex-rate-proxy >>& ~/.config/codex-rate-proxy/proxy.log &
echo $! >! ~/.config/codex-rate-proxy/proxy.pid
```

`config.ini`를 편집한 뒤 프로세스를 중단하지 않고 설정을 다시 읽습니다.

Bash:

```bash
kill -HUP "$(cat "$HOME/.config/codex-rate-proxy/proxy.pid")"
```

csh 또는 tcsh:

```csh
kill -HUP `cat ~/.config/codex-rate-proxy/proxy.pid`
```

API 서버 주소, 타임아웃, 요청 크기 제한, 속도 제한 정책, 포워드 프록시는
새 요청부터 적용합니다. 기존 요청과 SSE 스트림은 원래 설정으로 계속 처리합니다.
`[server] host` 또는 `port`를 변경하려면 재시작해야 합니다.
수정한 파일이 유효하지 않으면 오류를 로그에 기록하고 마지막 유효 설정을 유지합니다.

상태 확인:

```bash
curl http://127.0.0.1:8765/health
```

## Codex 설정

`~/.codex/config.toml`의 provider가 로컬 프록시를 바라보도록 설정합니다.
재시도 정책은 이 프록시가 담당하도록 Codex 측 재시도를 비활성화합니다.

```toml
model = "YOUR_MODEL"
model_provider = "corp"

[model_providers.corp]
name = "Corporate LLM"
base_url = "http://127.0.0.1:8765/v1"
wire_api = "responses"
env_key = "OPENAI_API_KEY"
requires_openai_auth = false
request_max_retries = 0
stream_max_retries = 0
supports_websockets = false
```

설정을 변경한 뒤 새 Codex 세션을 시작하세요.

## 기본 재시도 정책

기본 설정에서 API 서버로 보내는 일반 요청의 시작 간격은 최소 10초입니다.
429 응답을 받으면 약 5, 10, 20, 40, 60초 후에 재시도합니다.
`Retry-After`가 더 긴 대기 시간을 요구하면 그 시간을 우선 적용합니다.

요청 JSON과 응답 본문은 수정하지 않습니다.
프록시는 요청을 보내는 시점만 제어하고 최종 응답을 전달합니다.

## 보안 참고 사항

- 원격 접근을 의도한 경우가 아니라면 기본 수신 주소 `127.0.0.1`을 유지하세요.
- API 키나 내부 호스트 이름을 이 저장소에 넣지 마세요.
- 프록시 로그에는 프롬프트와 응답 본문을 기록하지 않습니다.
- Rust 구현은 HTTP API 서버와 선택적인 HTTP 포워드 프록시만 지원하도록 설계했습니다. TLS 스택은 포함하지 않습니다.

## 라이선스

아직 라이선스를 선택하지 않았습니다.
저장소가 공개되어 있어도 라이선스를 추가하기 전까지는 일반적인 저작권 규칙이 적용됩니다.
