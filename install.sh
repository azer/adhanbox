#!/bin/sh
# AdhanBox installer: finds the board on USB and flashes the latest release.
# No Rust or ESP-IDF needed; it downloads a prebuilt `espflash` into a temp folder.
#
#   curl -fsSL https://raw.githubusercontent.com/azer/adhanbox/main/install.sh | sh
#
# Options (pass after `sh -s --`):
#   --port PATH       use this serial port instead of auto-detecting
#   --version vX.Y.Z  install a specific release (default: latest)
#   --no-audio        skip the adhan audio (faster when updating firmware only)
#   --erase           wipe the whole flash first (settings included)
set -eu

REPO="azer/adhanbox"
ESPFLASH_VERSION="v4.7.0"
PORT=""
VERSION="latest"
AUDIO=1
ERASE=0

while [ $# -gt 0 ]; do
  case "$1" in
    --port) PORT="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --no-audio) AUDIO=0; shift ;;
    --erase) ERASE=1; shift ;;
    -h|--help) sed -n '2,13p' "$0" 2>/dev/null || true; exit 0 ;;
    *) echo "Unknown option: $1" >&2; exit 1 ;;
  esac
done

if [ -t 1 ]; then B="$(printf '\033[1m')"; G="$(printf '\033[32m')"; R="$(printf '\033[31m')"; N="$(printf '\033[0m')"; else B=""; G=""; R=""; N=""; fi
say()  { printf '%s==>%s %s\n' "$B" "$N" "$*"; }
die()  { printf '%serror:%s %s\n' "$R" "$N" "$*" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || die "'$1' is required but not installed."; }
need curl
need uname

# ── Pick the espflash build for this machine ────────────────────────────────
os="$(uname -s)"; arch="$(uname -m)"
case "$os" in
  Darwin)
    case "$arch" in
      arm64) target="aarch64-apple-darwin" ;;
      x86_64) target="x86_64-apple-darwin" ;;
      *) die "unsupported Mac CPU: $arch" ;;
    esac ;;
  Linux)
    case "$arch" in
      x86_64|amd64) target="x86_64-unknown-linux-musl" ;;
      aarch64|arm64) target="aarch64-unknown-linux-musl" ;;
      armv7l|armv7*) target="armv7-unknown-linux-gnueabihf" ;;
      *) die "unsupported Linux CPU: $arch" ;;
    esac ;;
  *) die "this script supports macOS and Linux. On Windows, use the browser installer: https://azer.github.io/adhanbox/" ;;
esac

tmp="$(mktemp -d 2>/dev/null || mktemp -d -t adhanbox)"
trap 'rm -rf "$tmp"' EXIT INT TERM

unzip_to() { # zip, dir
  if command -v unzip >/dev/null 2>&1; then unzip -q -o "$1" -d "$2"
  elif command -v python3 >/dev/null 2>&1; then python3 -m zipfile -e "$1" "$2"
  elif command -v bsdtar >/dev/null 2>&1; then bsdtar -xf "$1" -C "$2"
  else die "need 'unzip' (or python3) to unpack espflash"; fi
}

say "Downloading espflash $ESPFLASH_VERSION ($target)"
curl -fsSL -o "$tmp/espflash.zip" \
  "https://github.com/esp-rs/espflash/releases/download/$ESPFLASH_VERSION/espflash-$target.zip" \
  || die "could not download espflash"
unzip_to "$tmp/espflash.zip" "$tmp"
ESPFLASH="$tmp/espflash"
chmod +x "$ESPFLASH"
[ "$os" = Darwin ] && xattr -d com.apple.quarantine "$ESPFLASH" 2>/dev/null || true

# ── Download the firmware ───────────────────────────────────────────────────
if [ "$VERSION" = latest ]; then base="https://github.com/$REPO/releases/latest/download"
else base="https://github.com/$REPO/releases/download/$VERSION"; fi

say "Downloading AdhanBox ($VERSION)"
curl -fsSL -o "$tmp/flash.txt" "$base/flash.txt" || die "could not download the release from github.com/$REPO"
while read -r addr file; do
  [ -n "$file" ] || continue
  if [ "$file" = audio.bin ] && [ "$AUDIO" = 0 ]; then continue; fi
  printf '    %s\n' "$file"
  curl -fsSL -o "$tmp/$file" "$base/$file" || die "could not download $file"
done < "$tmp/flash.txt"

# ── Find the board ──────────────────────────────────────────────────────────
# ESP32-S3 native USB is Espressif VID 303a. macOS names it /dev/cu.usbmodem*, Linux /dev/ttyACM*.
list_ports() {
  if [ "$os" = Darwin ]; then
    for p in /dev/cu.usbmodem*; do [ -e "$p" ] && echo "$p"; done
  else
    for p in /dev/ttyACM* /dev/ttyUSB*; do
      [ -e "$p" ] || continue
      n="$(basename "$p")"
      v="$(cat "/sys/class/tty/$n/device/../idVendor" 2>/dev/null || true)"
      [ "$v" = 303a ] && echo "$p"
    done
  fi
  return 0
}

if [ -z "$PORT" ]; then
  ports="$(list_ports)"
  if [ -z "$ports" ]; then
    say "Plug in the AdhanBox board with a USB-C data cable (waiting up to 2 minutes)…"
    i=0
    while [ -z "$ports" ] && [ $i -lt 120 ]; do sleep 1; i=$((i + 1)); ports="$(list_ports)"; done
    [ -n "$ports" ] || die "no board found. Try another cable (some are charge-only) or another USB port."
    sleep 1
  fi
  count="$(printf '%s\n' "$ports" | wc -l | tr -d ' ')"
  if [ "$count" = 1 ]; then
    PORT="$ports"
  else
    echo "Several boards found:"
    printf '%s\n' "$ports" | awk '{ printf "  %d) %s\n", NR, $0 }'
    printf 'Which one? [1-%s] ' "$count"
    read -r pick < /dev/tty
    PORT="$(printf '%s\n' "$ports" | sed -n "${pick}p")"
    [ -n "$PORT" ] || die "no such choice"
  fi
fi
say "Using $PORT"

if [ "$os" = Linux ] && [ ! -w "$PORT" ]; then
  die "no permission to open $PORT. Run: sudo usermod -aG dialout \$USER, then log out and back in (or re-run with sudo)."
fi

ef() { "$ESPFLASH" "$@" --port "$PORT" --chip esp32s3 --non-interactive --skip-update-check; }

# ── Check it's the right board ──────────────────────────────────────────────
info="$(ef board-info 2>&1)" || { echo "$info"; die "could not talk to the board. Unplug it, plug it back in and run this again."; }
echo "$info" | grep -qi "esp32s3\|esp32-s3" || { echo "$info"; die "this is not an ESP32-S3 board."; }
if echo "$info" | grep -Eqi "flash size:? *(4 ?MB|2 ?MB)"; then
  echo "$info"; die "this board has less than 8 MB flash. AdhanBox needs the V2 Waveshare ESP32-S3-ePaper-1.54."
fi

# ── Flash ───────────────────────────────────────────────────────────────────
if [ "$ERASE" = 1 ]; then
  say "Erasing flash"
  ef erase-flash
fi

while read -r addr file; do
  [ -n "$file" ] || continue
  if [ "$file" = audio.bin ] && [ "$AUDIO" = 0 ]; then continue; fi
  say "Writing $file at $addr"
  ef write-bin "$addr" "$tmp/$file"
done < "$tmp/flash.txt"

printf '\n%sAdhanBox is installed.%s\n\n' "$G" "$N"
cat <<'EOF'
The board restarts on its own. On first start it walks you through setup:
  1. Press any button. A QR code appears on the screen.
  2. Scan it with your phone to join the "AdhanBox-XXXX" network.
  3. Pick your Wi-Fi (2.4 GHz) and your city on the page that opens. Done.

Connect a small 4-8 Ω speaker to the speaker header to hear the adhan.
EOF
