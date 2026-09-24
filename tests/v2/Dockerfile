# Isolated OpenCode 2.0.8 server + scripted OpenAI-compatible mock provider.
# Build context: tests/v2. No secrets are baked in; the Basic password is
# supplied per run through the container environment.
FROM python:3.12-slim-bookworm@sha256:392307d22300de8b5986851a12d9176dfc0fc073e65bf6523ebd7dcbeb23564e

ARG TARGETARCH
ARG OPENCODE_VERSION=2.0.8
# SHA-512 integrity values from https://registry.npmjs.org/@opencode/<pkg>/2.0.8 (dist.integrity).
ARG CLI_INTEGRITY=sha512-8Zm2aVylv44fXnUD9kqxzMmhowWAikTEOiIEwJiW0I87BeCbaj2f9zqy2WYQ5PatnbgGKLkluBFdFH2qoMWyNg==
ARG CLI_LINUX_ARM64_INTEGRITY=sha512-i1afIoAe79WLkaFSG2HwxVmXXxT2zbgC4IqP0srd+HD3cXS9veAgoEKnPFKW4Eb8eIrqQ+vJPh4D/PufOZ3CJw==
ARG CLI_LINUX_X64_INTEGRITY=sha512-PsvVkYTXVuDWP2ftDy0XlnKdyUeMS/XVORWpsqNXxOIvk1HMVNZ1rkUFgEqIenniOm2ECp4tQZfejaVuMqxWPw==

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl git ripgrep \
 && rm -rf /var/lib/apt/lists/*

COPY verify_integrity.py /harness/verify_integrity.py

RUN set -eu; \
    arch="${TARGETARCH:-$(dpkg --print-architecture)}"; \
    case "$arch" in \
      arm64) platform=linux-arm64; integrity="$CLI_LINUX_ARM64_INTEGRITY" ;; \
      amd64) platform=linux-x64; integrity="$CLI_LINUX_X64_INTEGRITY" ;; \
      *) echo "unsupported arch $arch" >&2; exit 1 ;; \
    esac; \
    cd /tmp; \
    curl -fsSL -o cli.tgz "https://registry.npmjs.org/@opencode/cli/-/cli-${OPENCODE_VERSION}.tgz"; \
    curl -fsSL -o platform.tgz "https://registry.npmjs.org/@opencode/cli-${platform}/-/cli-${platform}-${OPENCODE_VERSION}.tgz"; \
    python3 /harness/verify_integrity.py cli.tgz "$CLI_INTEGRITY"; \
    python3 /harness/verify_integrity.py platform.tgz "$integrity"; \
    mkdir -p /opt/opencode-meta /opt/opencode; \
    tar -xzf cli.tgz -C /opt/opencode-meta --strip-components=1; \
    tar -xzf platform.tgz -C /opt/opencode --strip-components=1; \
    rm cli.tgz platform.tgz; \
    python3 -c "import json,sys; v=json.load(open('/opt/opencode-meta/package.json'))['version']; sys.exit(v!='${OPENCODE_VERSION}')"; \
    python3 -c "import json,sys; v=json.load(open('/opt/opencode/package.json'))['version']; sys.exit(v!='${OPENCODE_VERSION}')"; \
    chmod 0755 /opt/opencode/bin/opencode; \
    test "$(HOME=/tmp /opt/opencode/bin/opencode --version)" = "opencode v${OPENCODE_VERSION}"; \
    rm -rf /tmp/.local /tmp/.config /tmp/.cache

RUN useradd --uid 1000 --create-home --shell /bin/bash harness

COPY mock_provider.py server_entry.sh opencode.json /harness/
COPY workspace /harness/workspace-seed
RUN chmod -R u=rwX,go=rX /harness && chmod 0755 /harness/server_entry.sh

ENV PYTHONUNBUFFERED=1
USER 1000:1000
WORKDIR /state
