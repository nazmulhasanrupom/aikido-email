#!/usr/bin/env bash
# Stops AIKIDO EMAIL. Asks before it deletes your data.
set -euo pipefail

DIR="${AIKIDO_DIR:-$HOME/aikido-email}"
[ -f "$DIR/docker-compose.yml" ] || { echo "Nothing found in $DIR." >&2; exit 1; }
cd "$DIR"

DOCKER="docker"
docker info >/dev/null 2>&1 || DOCKER="sudo docker"

$DOCKER compose down
echo "AIKIDO EMAIL is stopped. Your data is still here."

# read from the terminal, so it also works when the script is piped into bash
if [ -r /dev/tty ]; then
  read -r -p "Delete database too? (y/N) " answer </dev/tty
else
  answer="n"
fi
case "$answer" in
  y|Y|yes|YES)
    $DOCKER compose down -v
    cd "$HOME"
    rm -rf "$DIR"
    echo "Database and $DIR are deleted."
    ;;
  *) echo "Data kept in $DIR and in the Docker volumes." ;;
esac
