# Codex Rate Proxy

Codex CLI의 요청 간격과 HTTP 429 재시도를 제어하는 로컬 프록시입니다.
요청과 응답 본문은 변경하지 않으며, SSE 스트리밍을 그대로 전달합니다.

x86_64 CentOS 7.4용 정적 바이너리를 제공합니다. 실행 서버에 Rust·Python 설치나
root 권한이 필요하지 않습니다. 현재 Rust 버전은 **HTTP API와 HTTP 사내 프록시**를 지원합니다.

## 사용자 안내

관리자가 아래 설치와 공통 설정을 마친 뒤 사용하세요. 명령은 Bash와 csh/tcsh에서 동일합니다.

### 최초 등록 및 실행

```sh
# 최초 한 번 API 키 입력: 화면에 표시되지 않습니다.
codex-rate-proxy register hoony

# 등록한 사용자로 Codex 실행
codex-rate-proxy launch -u hoony

# -- 뒤에는 Codex 명령과 옵션을 그대로 전달
codex-rate-proxy launch -u hoony -- resume --last
codex-rate-proxy launch -u hoony -- --model YOUR_MODEL
```

`hoony` 대신 자신의 등록 이름을 사용하세요. 이름은 영문자·숫자·밑줄·하이픈 1~64자입니다.
`-u`와 `--user`는 동일합니다. 매번 자신의 이름을 지정해야 합니다.

키와 API 주소, 공통 설정 파일 경로가 같으면 기존 프록시를 재사용하고,
없으면 자동으로 포트를 할당합니다. Codex에 로컬 URL도 자동 전달하므로
사용자가 포트나 `base_url`을 따로 설정할 필요가 없습니다.

### 키 입력 방법 및 변경

```sh
# 직접 입력 대신 한 줄짜리 키 파일 또는 환경변수로 등록
codex-rate-proxy register hoony --key-file /path/to/my-key
codex-rate-proxy register hoony --key-env MY_LLM_KEY

# 등록된 키 교체
codex-rate-proxy register hoony --replace

# 등록 없이 실행: OPENAI_API_KEY 사용, 없으면 직접 입력
codex-rate-proxy launch
```

`launch`에서도 `--key-file PATH`, `--key-env NAME`, `--ask-key`,
`--key-stdin`을 사용할 수 있습니다. `-u`를 포함해 키 입력 방법은 하나만 선택하세요.
키를 교체해도 이미 실행 중인 세션은 기존 키를 사용합니다.

### URL 확인, 종료 및 등록 해제

```sh
# Codex 실행 없이 프록시를 준비하고 URL만 출력
codex-rate-proxy url -u hoony

# 자신의 미사용 프록시 종료
codex-rate-proxy stop -u hoony

# 저장된 사용자 등록 삭제
codex-rate-proxy unregister hoony
```

`stop`은 활성 세션이나 요청이 있으면 종료하지 않습니다.
`unregister`는 저장된 등록 정보만 삭제하며, 실행 중인 프록시를 종료하거나
API 제공자 측의 키를 폐기하지 않습니다.

## 관리자 안내

여기서 관리자는 공통 설정과 프로세스를 관리하는 사람을 뜻하며, root 권한은 필요하지 않습니다.

### 설치 및 업데이트

[최신 릴리스](https://github.com/hoony3000/codex-rate-proxy/releases/latest)에서
`codex-rate-proxy-x86_64-linux-musl.tar.gz`를 받아 실행 서버로 옮긴 뒤 설치합니다.
업데이트도 같은 방법이며, 기존 설정 파일은 보존됩니다.

```sh
tar -xzf codex-rate-proxy-x86_64-linux-musl.tar.gz
sh install.sh
```

| 항목 | 위치 |
| --- | --- |
| 실행 파일 | `~/.local/bin/codex-rate-proxy` |
| 공통 설정 | `~/.config/codex-rate-proxy/config.ini` |
| 등록된 키 | `~/.config/codex-rate-proxy/users/` |
| 암호화 마스터 키 | `~/.local/share/codex-rate-proxy/master.key` |
| 프록시 상태 및 로그 | `~/.local/state/codex-rate-proxy/` |

`~/.local/bin`을 PATH에 추가하세요. 지속 적용하려면 사용하는 셸의 초기화 파일에도 넣습니다.

```bash
# Bash (~/.bashrc)
export PATH="$HOME/.local/bin:$PATH"
```

```csh
# csh/tcsh (~/.cshrc)
set path = ( $HOME/.local/bin $path )
rehash
```

### 공통 INI 설정

설치된 `config.ini`에서 API 주소와 사내 프록시 주소를 실제 값으로 수정합니다.
API에 직접 연결하는 환경에서는 `[forward_proxy]`의 `http` 값을 비워 두세요.
사용자 키와 개인 키 파일 경로는 공통 INI에 넣지 않습니다.

```ini
[upstream]
base_url = http://llm.example.com/v1
timeout_seconds = 600
max_request_body_bytes = 134217728

[forward_proxy]
http = http://proxy.example.com:8080

[rate_limit]
min_interval_seconds = 10
max_retries = 5
backoff_base_seconds = 5
backoff_max_seconds = 60
backoff_jitter_seconds = 1

[launcher]
codex_binary = codex
provider = corp

[lifecycle]
idle_timeout_seconds = 1800
```

| 설정 | 의미 |
| --- | --- |
| `base_url` | 실제 API 주소. 예: `http://호스트/v1` |
| `timeout_seconds` | API 요청 타임아웃(초) |
| `max_request_body_bytes` | 요청 본문 최대 크기(기본 128 MiB) |
| `http` | 사내 HTTP 프록시 주소. 환경변수 HTTP_PROXY는 사용하지 않음 |
| `min_interval_seconds` | 요청 시작 사이의 최소 간격(초) |
| `max_retries` | 429 응답 후 최대 재시도 횟수 |
| `backoff_base_seconds` / `backoff_max_seconds` | 지수 백오프 시작값 / 상한(초). 더 긴 `Retry-After`는 우선 적용 |
| `backoff_jitter_seconds` | 백오프에 추가하는 무작위 대기 범위(초) |
| `codex_binary` | PATH상의 Codex 명령 또는 실행 파일 절대 경로 |
| `provider` | Codex 설정에 정의한 provider 이름 |
| `idle_timeout_seconds` | 세션과 요청이 모두 없는 프록시를 자동 종료하기까지의 시간(초) |

관리 모드에서는 포트를 자동 할당하므로 `[server] host/port`는 사용하지 않습니다.
키가 달라도 같은 API 계정의 제한을 공유하는 경우에는 이 프록시들이 그 제한을 합산하지 않습니다.
이 기능은 요청 간격과 429 대기를 제어하며, 정확한 TPM 제한 기능은 아닙니다.

### Codex 설정

`~/.codex/config.toml`에 사용할 모델과 provider를 정의합니다.

```toml
model = "YOUR_MODEL"
model_provider = "corp"

[model_providers.corp]
name = "Corporate LLM"
base_url = "http://llm.example.com/v1"
wire_api = "responses"
env_key = "OPENAI_API_KEY"
requires_openai_auth = false
request_max_retries = 0
stream_max_retries = 0
supports_websockets = false
```

`launch`는 실행하는 Codex에만 로컬 URL과 선택한 키, 재시도 설정을 전달합니다.
공통 `config.toml`과 부모 셸의 환경변수는 수정하지 않습니다.
위 API 주소를 직접 사용하는 `codex` 명령 대신 `codex-rate-proxy launch -u 이름`으로 실행하세요.

### 설정 반영 및 프록시 정리

INI를 저장하는 것만으로는 실행 중인 프록시에 반영되지 않습니다.
`list`에서 PID를 확인하고 `kill -HUP PID`로 다시 읽게 하세요.
유효한 설정은 새 요청부터 적용되고, 잘못된 설정이면 기존 값을 유지하며 로그에 오류를 남깁니다.
API 주소를 변경했다면 `launch`를 다시 실행하여 새 프록시를 만드세요.

```sh
codex-rate-proxy list
codex-rate-proxy prune --dry-run
codex-rate-proxy prune
```

`list`와 `prune`은 현재 HOME의 모든 관리 프록시를 대상으로 합니다.
`prune`은 활성 세션·요청이 없는 프록시만 종료합니다.
Codex가 입력을 기다리거나 도구를 실행하는 동안에는 사용 중으로 간주합니다.
평소에는 자동 정리에 맡기면 되며, 관리 모드는 별도 `nohup` 실행이 필요하지 않습니다.

같은 Linux 계정을 함께 쓰더라도 서로 다른 키는 별도 프록시를 사용합니다.
프로젝트마다 INI를 복사하면 같은 키도 별도 프록시가 생성되므로 공통 INI 하나를 사용하세요.

### 키 보관

등록된 키는 암호화하여 저장합니다. 복원이 필요하면 등록 파일과 마스터 키를 함께 안전하게 백업하세요.
같은 Linux 계정을 공유하는 사람은 두 파일에 모두 접근할 수 있으므로 사용자 간 보안 격리는 제공하지 않습니다.
키나 실제 내부 설정은 GitHub에 올리지 마세요.

v0.4.0의 평문 등록 파일을 일괄 암호화하려면 업그레이드 후 한 번 실행합니다.

```sh
codex-rate-proxy encrypt-keys
```
