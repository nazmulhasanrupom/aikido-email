#!/usr/bin/env bash
# AIKIDO EMAIL one-line installer.
#   curl -fsSL https://raw.githubusercontent.com/nazmulhasanrupom/aikido-email/main/install.sh | bash
# Safe to run again: it keeps your .env and your data.
set -euo pipefail

REPO="nazmulhasanrupom/aikido-email"
RAW="${AIKIDO_RAW_URL:-https://raw.githubusercontent.com/$REPO/main}"
DIR="${AIKIDO_DIR:-$HOME/aikido-email}"
ISSUES="https://github.com/$REPO/issues"

if [ -t 1 ]; then B=$'\033[1m'; G=$'\033[32m'; Y=$'\033[33m'; R=$'\033[31m'; C=$'\033[36m'; N=$'\033[0m'; else B=; G=; Y=; R=; C=; N=; fi
step() { printf '%s==>%s %s%s%s\n' "$C" "$N" "$B" "$*" "$N"; }
ok()   { printf '%s ok%s %s\n' "$G" "$N" "$*"; }
warn() { printf '%swarn%s %s\n' "$Y" "$N" "$*" >&2; }
die()  { printf '%serror%s %s\n' "$R" "$N" "$*" >&2; exit 1; }

printf '%s\n  AIKIDO EMAIL installer\n  Cold email automation. Self-hosted.%s\n\n' "$B" "$N"

# ---- 2. OS ----
case "$(uname -s)" in
  Linux)  OS=linux ;;
  Darwin) OS=mac ;;
  *) die "This installer works on Linux and macOS only. Your system: $(uname -s)." ;;
esac
step "System: $OS"

SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  command -v sudo >/dev/null 2>&1 && SUDO="sudo"
fi
need_root() { [ "$(id -u)" -eq 0 ] || [ -n "$SUDO" ] || die "I need root rights for this step, and sudo is not installed. Run as root."; }

# ---- 3. curl and openssl ----
pkg_install() {
  if command -v apt-get >/dev/null 2>&1; then need_root; $SUDO apt-get update -qq && $SUDO apt-get install -y -qq "$@"
  elif command -v dnf >/dev/null 2>&1; then need_root; $SUDO dnf install -y -q "$@"
  elif command -v yum >/dev/null 2>&1; then need_root; $SUDO yum install -y -q "$@"
  elif command -v pacman >/dev/null 2>&1; then need_root; $SUDO pacman -S --noconfirm --needed "$@"
  elif command -v apk >/dev/null 2>&1; then need_root; $SUDO apk add --no-cache "$@"
  elif command -v brew >/dev/null 2>&1; then brew install "$@"
  else die "No known package manager. Install this by hand: $*"; fi
}
for tool in curl openssl; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    step "Installing $tool"
    pkg_install "$tool"
  fi
done
ok "curl and openssl are ready"

# ---- 4. Docker ----
if ! command -v docker >/dev/null 2>&1; then
  if [ "$OS" = mac ]; then
    die "Docker is missing. Install Docker Desktop: https://www.docker.com/products/docker-desktop/ . Start it. Then run this installer again."
  fi
  step "Installing Docker (this takes a minute)"
  need_root
  curl -fsSL https://get.docker.com | $SUDO sh
  if command -v systemctl >/dev/null 2>&1; then
    $SUDO systemctl enable --now docker || warn "Could not start the Docker service with systemctl."
  fi
  if [ "$(id -u)" -ne 0 ]; then
    $SUDO usermod -aG docker "$USER" || warn "Could not add $USER to the docker group."
    warn "You were added to the docker group. Log out and in again to use docker without sudo."
  fi
fi
ok "Docker is installed"

# ---- 6. sudo for docker when needed ----
DOCKER="docker"
if ! docker info >/dev/null 2>&1; then
  if [ -n "$SUDO" ] && $SUDO docker info >/dev/null 2>&1; then
    DOCKER="$SUDO docker"
    warn "Using sudo for docker in this run."
  else
    die "Docker is installed but not running or not allowed. On macOS, start Docker Desktop. On Linux, run: sudo systemctl start docker"
  fi
fi

# ---- 5. docker compose v2 ----
if ! $DOCKER compose version >/dev/null 2>&1; then
  [ "$OS" = linux ] || die "docker compose is missing. Update Docker Desktop."
  step "Installing the docker compose plugin"
  pkg_install docker-compose-plugin 2>/dev/null || pkg_install docker-compose || true
  $DOCKER compose version >/dev/null 2>&1 || die "Could not install docker compose v2. See https://docs.docker.com/compose/install/linux/"
fi
ok "docker compose is ready"

# ---- 7. folder and compose file ----
step "Folder: $DIR"
mkdir -p "$DIR"
cd "$DIR"
curl -fsSL "$RAW/docker-compose.install.yml" -o docker-compose.yml.new || die "Could not download docker-compose.yml from $RAW"
mv docker-compose.yml.new docker-compose.yml
ok "docker-compose.yml saved"

# ---- 8 + 9. .env (never overwritten) ----
port_busy() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
host_for_url() {
  local ip=""
  [ "$OS" = linux ] && ip="$(hostname -I 2>/dev/null | awk '{print $1}')" || true
  echo "${ip:-localhost}"
}
if [ -f .env ]; then
  ok ".env already exists. I keep it as it is."
else
  step "Making .env with random secrets"
  PORT=8080
  while port_busy "$PORT"; do PORT=$((PORT + 1)); done
  [ "$PORT" = 8080 ] || warn "Port 8080 is busy. Using port $PORT."
  ( umask 077
    cat > .env <<ENVEOF
POSTGRES_USER=aikido
POSTGRES_PASSWORD=$(openssl rand -hex 24)
POSTGRES_DB=aikido
AIKIDO_ENCRYPTION_KEY=$(openssl rand -base64 32)
APP_PORT=$PORT
AIKIDO_PUBLIC_URL=http://$(host_for_url):$PORT
AIKIDO_INSECURE_COOKIES=true
ENVEOF
  )
  ok ".env saved. Back up AIKIDO_ENCRYPTION_KEY from it."
fi
PORT="$(grep -E '^APP_PORT=' .env | tail -n1 | cut -d= -f2)"
PORT="${PORT:-8080}"
URL="$(grep -E '^AIKIDO_PUBLIC_URL=' .env | tail -n1 | cut -d= -f2-)"
URL="${URL:-http://localhost:$PORT}"

# ---- 10. start ----
step "Downloading the images"
$DOCKER compose pull
step "Starting"
$DOCKER compose up -d

# ---- 11. wait for health ----
step "Waiting for the app (up to 60 seconds)"
healthy=0
for _ in $(seq 1 30); do
  if curl -fsS "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then healthy=1; break; fi
  sleep 2
done

# ---- 12 + 13. result ----
if [ "$healthy" = 1 ]; then
  printf '\n%sAIKIDO EMAIL is running.%s\n\n  Open: %s%s%s\n\n' "$G$B" "$N" "$B" "$URL" "$N"
  printf '  The first account you make becomes the owner.\n\n'
  printf '  stop:    cd %s && docker compose down\n' "$DIR"
  printf '  update:  cd %s && docker compose pull && docker compose up -d\n' "$DIR"
  printf '  logs:    cd %s && docker compose logs -f app\n\n' "$DIR"
  warn "Login runs over plain HTTP. Use it on a trusted network, or put HTTPS in front."
else
  printf '\n%sThe app did not become healthy in 60 seconds.%s Last 50 log lines:\n\n' "$R$B" "$N" >&2
  $DOCKER compose logs --tail 50 >&2 || true
  printf '\nNeed help? %s\n' "$ISSUES" >&2
  exit 1
fi
