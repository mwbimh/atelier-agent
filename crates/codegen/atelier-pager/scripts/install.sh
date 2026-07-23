#!/bin/bash
#
# Atelier CLI installer — https://atelier/cli/install.sh
#
# Env: ATELIER_RELEASE_BASE_URL, ATELIER_CHANNEL (stable|alpha|enterprise,
# default: stable), ATELIER_BIN_DIR
#
# Usage:
#   curl -fsSL https://atelier/cli/install.sh | bash            # latest stable
#   curl -fsSL https://atelier/cli/install.sh | bash -s 0.1.42  # specific version
#   ATELIER_RELEASE_BASE_URL=https://releases.example/atelier bash install.sh
#
# Windows: run under Git for Windows / MSYS2 Bash (same curl | bash flow); WSL
# uses the Linux binary.

set -e

: "${ATELIER_RELEASE_BASE_URL:?ATELIER_RELEASE_BASE_URL must point to the Atelier release directory}"
BASE_URL="${ATELIER_RELEASE_BASE_URL%/}"

TARGET="$1"

if [[ -n "$TARGET" ]] && [[ ! "$TARGET" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9._]+)?$ ]]; then
    echo "Invalid version format: $TARGET (expected X.Y.Z or X.Y.Z-suffix)" >&2
    exit 1
fi

DOWNLOADER=""
if command -v curl >/dev/null 2>&1; then
    DOWNLOADER="curl"
elif command -v wget >/dev/null 2>&1; then
    DOWNLOADER="wget"
else
    echo "Either curl or wget is required but neither is installed" >&2
    exit 1
fi

download_file() {
    local url="$1" output="$2"
    if [ "$DOWNLOADER" = "curl" ]; then
        if [ -n "$output" ]; then
            curl -fsSL -o "$output" "$url"
        else
            curl -fsSL "$url"
        fi
    else
        if [ -n "$output" ]; then
            wget -q -O "$output" "$url"
        else
            wget -q -O - "$url"
        fi
    fi
}

# Parallel byte-range download. Falls back to single-connection download_file
# whenever HEAD lacks Content-Length, the file is small (<16 MiB), curl is
# unavailable, or any chunk fetch / concat fails.
download_file_parallel() {
    local url="$1" output="$2"
    if [ "$DOWNLOADER" != "curl" ]; then
        download_file "$url" "$output"
        return
    fi
    local size
    size=$(curl -fsSL --head "$url" 2>/dev/null | awk -F'[: \r\n]+' 'tolower($1)=="content-length"{print $2; exit}')
    if [ -z "$size" ] || ! [ "$size" -ge 16777216 ] 2>/dev/null; then
        download_file "$url" "$output"
        return
    fi
    local n=8
    local chunk_size=$(( (size + n - 1) / n ))
    local tmpdir
    tmpdir=$(mktemp -d 2>/dev/null) || { download_file "$url" "$output"; return; }
    local pids=() i start end
    for i in $(seq 0 $((n - 1))); do
        start=$((i * chunk_size))
        end=$((start + chunk_size - 1))
        [ $end -ge $size ] && end=$((size - 1))
        curl -fsSL -r "${start}-${end}" -o "${tmpdir}/$(printf 'chunk.%03d' "$i")" "$url" &
        pids+=($!)
    done
    local all_ok=true pid
    for pid in "${pids[@]}"; do
        wait "$pid" || all_ok=false
    done
    if [ "$all_ok" = true ] && cat "${tmpdir}"/chunk.* > "$output" 2>/dev/null; then
        rm -rf "$tmpdir"
        return 0
    fi
    rm -rf "$tmpdir"
    download_file "$url" "$output"
}

# Return 0 if a HEAD request for the URL gets HTTP 404.
is_not_found() {
    local url="$1" code
    if [ "$DOWNLOADER" = "curl" ]; then
        code=$(curl -o /dev/null -sSL -w '%{http_code}' --head "$url" 2>/dev/null) || true
    else
        code=$(wget --server-response --spider "$url" 2>&1 | awk '/HTTP\//{print $2}' | tail -1) || true
    fi
    [ "$code" = "404" ]
}

case "$(uname -s)" in
    Darwin) os="macos" ;;
    Linux)  os="linux" ;;
    # Git for Windows / MSYS2 / Cygwin host — native Windows builds
    MINGW* | MSYS* | CYGWIN*) os="windows" ;;
    *)      echo "Unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac

case "$(uname -m)" in
    x86_64|amd64|AMD64) arch="x86_64" ;;
    arm64|aarch64|ARM64) arch="aarch64" ;;
    *)                    echo "Unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

DOWNLOAD_DIR="$HOME/.atelier/downloads"
BIN_DIR="${ATELIER_BIN_DIR:-$HOME/.atelier/bin}"
mkdir -p "$DOWNLOAD_DIR" "$BIN_DIR"

platform="${os}-${arch}"
CHANNEL="${ATELIER_CHANNEL:-stable}"

probe_result=""
if [ -z "$TARGET" ]; then
    echo "Fetching latest ${CHANNEL} version..." >&2
    probe_result=$(download_file "${BASE_URL}/${CHANNEL}" 2>/dev/null) || true
fi

if [ -n "$TARGET" ]; then
    version="$TARGET"
else
    version=$(printf '%s' "$probe_result" | tr -d '\r' | head -n1 | tr -d '[:space:]')
    if [ -z "$version" ]; then
        echo "Error: failed to fetch latest version from ${BASE_URL}/${CHANNEL}" >&2
        exit 1
    fi
fi

if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9._]+)?$ ]]; then
    echo "Invalid version format: $version (expected X.Y.Z or X.Y.Z-suffix)" >&2
    exit 1
fi

echo "Installing Atelier $version ($platform)..." >&2

binary_path="$DOWNLOAD_DIR/atelier-$platform"
artifact_base="${BASE_URL}/atelier-${version}-${platform}"

if [ "$os" = "windows" ]; then
    binary_path="${binary_path}.exe"
fi

binary_tmp="${binary_path}.tmp.$$"
rm -f "$binary_tmp" 2>/dev/null || true

echo "  Downloading atelier ${version}..." >&2
if [ "$os" = "windows" ]; then
    if ! download_file_parallel "${artifact_base}.exe" "$binary_tmp"; then
        if ! download_file_parallel "$artifact_base" "$binary_tmp"; then
            rm -f "$binary_tmp"
            if is_not_found "${artifact_base}.exe"; then
                echo "Error: Atelier is not yet available for your system ($platform)." >&2
            else
                echo "Error: binary download failed (${artifact_base}.exe and ${artifact_base})" >&2
            fi
            exit 1
        fi
    fi
elif ! download_file_parallel "$artifact_base" "$binary_tmp"; then
    rm -f "$binary_tmp"
    if is_not_found "$artifact_base"; then
        echo "Error: Atelier is not yet available for your system ($platform)." >&2
    else
        echo "Error: binary download failed from ${artifact_base}" >&2
    fi
    exit 1
fi

if [ "$os" = "windows" ]; then
    mv -f "$binary_tmp" "$binary_path"
    # Symlinks require Developer Mode on Windows; copy instead.
    # If the exe is locked by a running process, rename it aside then retry.
    for bin_name in atelier.exe agent.exe; do
        rm -f "$BIN_DIR/$bin_name.old" 2>/dev/null || true  # stale backup from prior update
        if ! cp -f "$binary_path" "$BIN_DIR/$bin_name" 2>/dev/null; then
            mv -f "$BIN_DIR/$bin_name" "$BIN_DIR/$bin_name.old" 2>/dev/null || true
            if ! cp -f "$binary_path" "$BIN_DIR/$bin_name" 2>/dev/null; then
                # Rollback: restore the old binary so the install isn't broken.
                mv -f "$BIN_DIR/$bin_name.old" "$BIN_DIR/$bin_name" 2>/dev/null || true
                echo "Error: failed to install $bin_name" >&2
                exit 1
            fi
        fi
    done
    echo "  Binary installed to $BIN_DIR/atelier.exe and $BIN_DIR/agent.exe." >&2
else
    chmod +x "$binary_tmp"
    if ! "$binary_tmp" --version </dev/null >/dev/null 2>&1; then
        echo "Error: downloaded atelier failed to run; keeping the existing install." >&2
        rm -f "$binary_tmp"
        exit 1
    fi
    mv -f "$binary_tmp" "$binary_path"
    # Use relative symlinks when BIN_DIR and DOWNLOAD_DIR share a parent
    # (default layout: ~/.atelier/bin and ~/.atelier/downloads are siblings).
    # Relative symlinks survive Docker bind-mounts with a different $HOME.
    if [ "$(dirname "$BIN_DIR")" = "$(dirname "$DOWNLOAD_DIR")" ]; then
        link_target="../$(basename "$DOWNLOAD_DIR")/$(basename "$binary_path")"
    else
        link_target="$binary_path"
    fi
    ln -sf "$link_target" "$BIN_DIR/atelier"
    ln -sf "$link_target" "$BIN_DIR/agent"
    echo "  Binary linked to $BIN_DIR/atelier and $BIN_DIR/agent." >&2
fi

# Generate shell completions (best-effort)
mkdir -p "$HOME/.atelier/completions/bash" "$HOME/.atelier/completions/zsh"
"$BIN_DIR/atelier" completions bash > "$HOME/.atelier/completions/bash/atelier.bash" 2>/dev/null || true
"$BIN_DIR/atelier" completions zsh  > "$HOME/.atelier/completions/zsh/_atelier"     2>/dev/null || true
# Fish: write to the auto-loaded completions dir so it works immediately
if mkdir -p "$HOME/.config/fish/completions" 2>/dev/null; then
    "$BIN_DIR/atelier" completions fish > "$HOME/.config/fish/completions/atelier.fish" 2>/dev/null || true
fi

# Persist installer source and channel to config
CONFIG_FILE="$HOME/.atelier/config.toml"
CLI_BLOCK="installer = \"internal\""
if [ "$CHANNEL" != "stable" ]; then
    CLI_BLOCK="${CLI_BLOCK}\nchannel = \"${CHANNEL}\""
fi
if [ ! -f "$CONFIG_FILE" ]; then
    printf '[cli]\n%b\n' "$CLI_BLOCK" > "$CONFIG_FILE"
elif grep -q '^\[cli\]' "$CONFIG_FILE"; then
    tmp="$CONFIG_FILE.tmp.$$"
    awk -v block="$CLI_BLOCK" '
        /^\[cli\][[:space:]]*(#.*)?$/ { print; printf "%s\n", block; in_cli=1; next }
        /^\[.*\][[:space:]]*(#.*)?$/  { in_cli=0 }
        in_cli && /^[[:space:]]*(installer|channel)[[:space:]]*=/ { next }
        { print }
    ' "$CONFIG_FILE" > "$tmp" && mv "$tmp" "$CONFIG_FILE"
else
    printf '\n[cli]\n%b\n' "$CLI_BLOCK" >> "$CONFIG_FILE"
fi

if [ "$os" = "windows" ]; then
    echo "Atelier $version installed to $BIN_DIR/atelier.exe" >&2
else
    echo "Atelier $version installed to $BIN_DIR/atelier" >&2
fi

# --- Ensure atelier is on PATH ---

path_has_dir() {
    case ":$PATH:" in *":$1:"*) return 0 ;; *) return 1 ;; esac
}

# Try to symlink into a directory already on PATH so atelier works immediately
# without restarting the shell. Candidate dirs in preference order.
SYMLINK_CREATED=""
if [ "$os" != "windows" ] && ! path_has_dir "$BIN_DIR"; then
    for candidate in "$HOME/.local/bin" "/usr/local/bin"; do
        if path_has_dir "$candidate" && [ -d "$candidate" ] && [ -w "$candidate" ]; then
            ln -sf "$BIN_DIR/atelier" "$candidate/atelier"
            ln -sf "$BIN_DIR/agent" "$candidate/agent"
            SYMLINK_CREATED="$candidate"
            echo "  Symlinked $candidate/atelier -> $BIN_DIR/atelier" >&2
            echo "  Symlinked $candidate/agent -> $BIN_DIR/agent" >&2
            break
        fi
    done
fi

# Also update shell config so ~/.atelier/bin is on PATH for future sessions
user_shell="$(basename "${SHELL:-}")"
config_file=""

case "$user_shell" in
    bash) config_file="$HOME/.bashrc" ;;
    zsh)  config_file="$HOME/.zshrc" ;;
    fish) config_file="$HOME/.config/fish/config.fish" ;;
esac

if [ -n "$config_file" ]; then
    mkdir -p "$(dirname "$config_file")"

    # Resolve symlinks so tmp+mv rewrites the stow/dotfiles target, not the link.
    if [ -e "$config_file" ] || [ -L "$config_file" ]; then
        _cf="$config_file"
        _depth=0
        while [ -L "$_cf" ] && [ "$_depth" -lt 40 ]; do
            _link="$(readlink "$_cf")" || break
            case "$_link" in
                /*) _cf="$_link" ;;
                *)  _cf="$(cd "$(dirname "$_cf")" && pwd -P)/$_link" ;;
            esac
            _depth=$((_depth + 1))
        done
        # Still a symlink (cycle/cap): leave original path so we never rewrite the link.
        if [ ! -L "$_cf" ]; then
            config_file="$(cd "$(dirname "$_cf")" && pwd -P)/$(basename "$_cf")"
        fi
        unset _cf _link _depth
    fi

    # Build the new installer block
    if [ "$user_shell" = "fish" ]; then
        new_block='# >>> atelier installer >>>
fish_add_path $HOME/.atelier/bin
# <<< atelier installer <<<'
    elif [ "$user_shell" = "zsh" ]; then
        new_block='# >>> atelier installer >>>
export PATH="$HOME/.atelier/bin:$PATH"
fpath=(~/.atelier/completions/zsh $fpath)
autoload -Uz compinit && compinit -C
# <<< atelier installer <<<'
    else
        new_block='# >>> atelier installer >>>
export PATH="$HOME/.atelier/bin:$PATH"
[[ -r "$HOME/.atelier/completions/bash/atelier.bash" ]] && source "$HOME/.atelier/completions/bash/atelier.bash"
# <<< atelier installer <<<'
    fi

    if grep -qs "atelier installer" "$config_file" 2>/dev/null; then
        # Replace existing block in-place (strip old >>> to <<< lines, insert new)
        tmp="$config_file.tmp.$$"
        awk '
            /# >>> atelier installer >>>/ { skip=1; next }
            /# <<< atelier installer <<</ { skip=0; next }
            !skip { print }
        ' "$config_file" > "$tmp" && mv "$tmp" "$config_file"
    else
        [ -f "$config_file" ] && cp "$config_file" "$config_file.bak.$(date +%s)"

        # macOS bash: ensure bash_profile sources bashrc
        if [ "$user_shell" = "bash" ] && [ "$(uname -s)" = "Darwin" ]; then
            if [ -f "$HOME/.bash_profile" ] && ! grep -qs "source ~/.bashrc" "$HOME/.bash_profile"; then
                printf '\n[[ -r ~/.bashrc ]] && source ~/.bashrc\n' >> "$HOME/.bash_profile"
            fi
        fi
    fi

    printf '\n%s\n' "$new_block" >> "$config_file"
    echo "  Updated $BIN_DIR in PATH in $config_file." >&2
fi

echo "" >&2
if path_has_dir "$BIN_DIR" || [ -n "$SYMLINK_CREATED" ]; then
    echo "Run 'atelier' or 'agent' to get started!" >&2
elif [ -n "$config_file" ]; then
    echo "Restart your terminal, then run 'atelier' or 'agent' to get started!" >&2
else
    echo "Add $BIN_DIR to your PATH, then run 'atelier' or 'agent' to get started:" >&2
    echo '  export PATH="$HOME/.atelier/bin:$PATH"' >&2
fi

if [ "$os" = "windows" ]; then
    echo "To use atelier from cmd.exe or PowerShell, add %USERPROFILE%\\.atelier\\bin to your PATH." >&2
fi
