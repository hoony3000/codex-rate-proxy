# 공용 CentOS 계정에서 Claude Code 키를 사용자별로 설정

같은 Linux 계정과 프로젝트를 쓰면서도 등록 키를 다르게 선택할 수 있습니다.
사용자별 Remote‑SSH 서버 데이터 폴더에 Claude 설정을 두고, 공용 `claude-ide.sh`에
등록 이름별 링크를 만듭니다. 같은 Linux UID의 사용자끼리 파일 접근을 차단하는 구성은 아닙니다.

### CentOS에서 래퍼 설치

등록된 키는 사내 Claude 호환 엔드포인트에서 유효해야 합니다. Codex와 Claude의 키가 다르면
`hoony-claude`처럼 별도 이름으로 한 번 등록하고 링크에도 그 이름을 사용합니다.

```sh
mkdir -p ~/.local/bin
install -m 700 claude-ide.sh ~/.local/bin/claude-ide
ln -sfnT claude-ide ~/.local/bin/claude-ide-hoony
```

이 스크립트는 설치된 `~/.local/bin/codex-rate-proxy` v0.7.0 이상에서 키를 읽습니다.
Claude 확장이 실행 파일을 넘기면 그 파일을 사용합니다. CentOS 호환용 Claude를 별도로
설치했다면 아래 환경변수 목록에 `CLAUDE_IDE_BINARY`와 그 실행 파일의 절대 경로를 넣으세요.
확장이 번들 경로를 넘기지 않는 경우의 기본 경로는 `~/.local/bin/claude`입니다.

### 사용자별 Remote‑SSH 서버 설정

로컬 PC의 SSH 설정에서 같은 호스트와 계정을 사용하되 사람마다 고유한 별칭을 지정합니다.

```sshconfig
Host centos-claude-hoony
    HostName centos.example.com
    User shared
```

로컬 VS Code **User Settings (JSON)**에 해당 별칭의 전용 설치 위치를 추가합니다. 오프라인
CentOS 번들을 쓰는 예시이며, 기존 설정이 있으면 맵에 항목만 병합하세요.

```json
{
  "remote.SSH.useExecServer": false,
  "remote.SSH.serverInstallPath": {
    "centos-claude-hoony": "/home/shared/.vscode-remote-hoony"
  }
}
```

오프라인 서버에서는 새 위치에도 **현재 VS Code와 동일한 커밋의 패치된 서버**가 있어야 합니다.
Remote‑SSH 출력에서 해당 별칭이 사용하는 실제 설치 위치와 커밋을 확인하세요. 설치 위치 아래
`bin/<커밋>`에서 기존 CentOS 호환 서버를 찾을 수 있도록 준비한 후 접속합니다.
이미 설치된 같은 커밋을 복사한다면 서버의 Node가 원래 폴더의 런타임 로더를 참조할 수 있으므로
원본 폴더를 유지하고, 복사본의 `node --version`이 동작하는지 확인하세요. Claude Code 확장이
새 위치에 없다면 **Install from VSIX**로 원격 호스트에 설치합니다.

전용 별칭으로 접속한 뒤 **Preferences: Open Remote Settings (JSON)**을 엽니다.
서버 데이터 폴더가 사람마다 다른지 확인한 뒤 아래 값을 넣으세요.
`/home/shared`를 실제 경로로, URL은 사내에서 사용하도록 승인된 값으로 바꿉니다.

```json
{
  "claudeCode.claudeProcessWrapper": "/home/shared/.local/bin/claude-ide-hoony",
  "claudeCode.disableLoginPrompt": true,
  "claudeCode.environmentVariables": [
    { "name": "ANTHROPIC_BASE_URL", "value": "http://claude-gateway.example.com:8000" }
  ]
}
```

- 인증 방식이 Bearer라면 기본값으로 두고, 게이트웨이가 `x-api-key`를 요구하면 배열에
  `{ "name": "CLAUDE_IDE_AUTH_HEADER", "value": "x-api-key" }`를 추가합니다.
- 사내 API가 OpenAI Responses만 지원한다면 Claude에서 그대로 사용할 수 없습니다.
  `ANTHROPIC_BASE_URL`에는 Anthropic 호환 `/v1/messages`를 제공하는 서버의 기준 주소를
  넣습니다. 기존 rate proxy의 upstream도 Messages를 제공한다면 공통 게이트웨이 주소
  `http://127.0.0.1:8765`와 기본 Bearer 인증을 사용할 수 있습니다.
- 서버 데이터 폴더를 분리하지 않은 상태에서 공용 **Remote Settings**에 한 사람의 래퍼를
  지정하면 다른 사용자도 같은 키를 사용하게 됩니다.

설정 후 **Developer: Reload Window**를 실행하고 새 Claude 대화로 확인합니다.
