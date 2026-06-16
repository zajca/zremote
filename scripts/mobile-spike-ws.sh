#!/usr/bin/env bash
# Phase 0 WebSocket spike runner.
#
# Orchestrates: docker build -> swift build (cross-compiled for Android) ->
# adb push binary + runtime libs to /data/local/tmp/zremote-spike -> adb shell
# run -> stream output back with a SPIKE: grep filter.
#
# Exit codes:
#   0  spike ran and verdict was GREEN or YELLOW (informational YELLOW still
#      exits 0 so CI does not block on "agent not running")
#   1  build failed (docker build or swift build)
#   2  no adb device attached (after waiting briefly)
#   3  runtime WebSocket probe returned RED verdict
#   4  prerequisite missing (docker, adb)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
SPIKE_DIR="${REPO_ROOT}/mobile/spike"
OUT_DIR="${SPIKE_DIR}/out"
IMAGE_TAG="zremote-mobile-spike:6.3"
DEVICE_TMP="/data/local/tmp/zremote-spike"

SKIP_BUILD=0
NO_RUN=0
DEVICE=""
SPIKE_MINUTES="${SPIKE_MINUTES:-5}"
ZREMOTE_WS_URL_ARG=""

usage() {
    cat <<EOF
Usage: $0 [flags]

Flags:
  --skip-build         Do not rebuild the Docker image or re-run swift build
  --device <adb-id>    Target a specific adb device
  --no-run             Build only, do not push or execute the spike
  --minutes N          Override soak duration (default ${SPIKE_MINUTES})
  --ws-url URL         Override ZREMOTE_WS_URL passed to the spike
  -h, --help           Show this help
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-build) SKIP_BUILD=1; shift ;;
        --no-run) NO_RUN=1; shift ;;
        --device) DEVICE="$2"; shift 2 ;;
        --minutes) SPIKE_MINUTES="$2"; shift 2 ;;
        --ws-url) ZREMOTE_WS_URL_ARG="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown flag: $1" >&2; usage; exit 1 ;;
    esac
done

log() {
    printf '[mobile-spike-ws] %s\n' "$*"
}

fail() {
    printf '[mobile-spike-ws] FATAL: %s\n' "$*" >&2
    exit "${2:-1}"
}

# --- prerequisites ---------------------------------------------------------

command -v docker >/dev/null 2>&1 || fail "docker is not on PATH" 4
command -v adb >/dev/null 2>&1 || fail "adb is not on PATH (nix Android SDK provides it)" 4

# --- build ------------------------------------------------------------------

if [[ "${SKIP_BUILD}" -eq 0 ]]; then
    log "building docker image ${IMAGE_TAG}"
    docker build -t "${IMAGE_TAG}" "${SPIKE_DIR}"

    log "running swift build inside the container"
    mkdir -p "${OUT_DIR}"
    docker run --rm \
        -v "${SPIKE_DIR}:/workspace" \
        -w /workspace \
        "${IMAGE_TAG}" \
        bash /workspace/scripts/build-in-docker.sh
else
    log "--skip-build set; assuming ${OUT_DIR}/WsSpike already exists"
fi

if [[ ! -f "${OUT_DIR}/WsSpike" ]]; then
    fail "expected binary ${OUT_DIR}/WsSpike was not produced" 1
fi
log "binary: $(file "${OUT_DIR}/WsSpike")"

if [[ "${NO_RUN}" -eq 1 ]]; then
    log "--no-run set; build complete, not pushing to device"
    exit 0
fi

# --- device selection -------------------------------------------------------

adb start-server >/dev/null 2>&1 || true

DEVICE_COUNT=$(adb devices | awk 'NR>1 && $2=="device"' | wc -l)
if [[ "${DEVICE_COUNT}" -eq 0 ]]; then
    log "no adb device found. Attach a phone via USB (with USB debugging on) or boot an emulator, then re-run with --skip-build."
    exit 2
fi

ADB_ARGS=()
if [[ -n "${DEVICE}" ]]; then
    ADB_ARGS=(-s "${DEVICE}")
fi

log "using device: $(adb ${ADB_ARGS[@]+"${ADB_ARGS[@]}"} get-serialno 2>/dev/null || echo unknown)"

# --- reverse proxy so the device can reach the host-side zremote agent ------

# The spike defaults to ws://10.0.2.2:3000/ws/events which works on Android
# emulators. For physical devices the reliable trick is `adb reverse`, which
# forwards a socket on the device to a host-side port. Set up both — it is
# idempotent.
log "setting adb reverse tcp:3000 -> tcp:3000 (so device can reach a local zremote agent)"
adb ${ADB_ARGS[@]+"${ADB_ARGS[@]}"} reverse tcp:3000 tcp:3000 >/dev/null 2>&1 || \
    log "adb reverse failed (expected on unrooted emulators); will rely on 10.0.2.2"

# --- push artifacts --------------------------------------------------------

log "preparing ${DEVICE_TMP} on device"
adb ${ADB_ARGS[@]+"${ADB_ARGS[@]}"} shell "rm -rf ${DEVICE_TMP} && mkdir -p ${DEVICE_TMP}/lib"

log "pushing spike binary"
adb ${ADB_ARGS[@]+"${ADB_ARGS[@]}"} push "${OUT_DIR}/WsSpike" "${DEVICE_TMP}/WsSpike" >/dev/null
adb ${ADB_ARGS[@]+"${ADB_ARGS[@]}"} shell "chmod 755 ${DEVICE_TMP}/WsSpike"

if [[ -d "${OUT_DIR}/lib" ]] && [[ -n "$(ls -A "${OUT_DIR}/lib" 2>/dev/null || true)" ]]; then
    log "pushing runtime libs ($(ls "${OUT_DIR}/lib" | wc -l) files)"
    for f in "${OUT_DIR}/lib"/*; do
        adb ${ADB_ARGS[@]+"${ADB_ARGS[@]}"} push "${f}" "${DEVICE_TMP}/lib/" >/dev/null
    done
else
    log "no runtime libs to push (hopefully the SDK's shipped libs are available via default LD_LIBRARY_PATH)"
fi

# --- run --------------------------------------------------------------------

WS_URL="${ZREMOTE_WS_URL_ARG:-ws://127.0.0.1:3000/ws/events}"
log "running spike on device (soak ${SPIKE_MINUTES}m, ws=${WS_URL})"
log "---- device stdout begins ----"
set +e
adb ${ADB_ARGS[@]+"${ADB_ARGS[@]}"} shell \
    "cd ${DEVICE_TMP} && LD_LIBRARY_PATH=${DEVICE_TMP}/lib ZREMOTE_WS_URL='${WS_URL}' SPIKE_MINUTES='${SPIKE_MINUTES}' ${DEVICE_TMP}/WsSpike"
RC=$?
set -e
log "---- device stdout ends (exit=${RC}) ----"

case "${RC}" in
    0) log "spike reports GREEN/YELLOW verdict"; exit 0 ;;
    3) log "spike reports RED verdict — WebSocket probe failed on device"; exit 3 ;;
    *) log "spike exited with unexpected code ${RC}"; exit "${RC}" ;;
esac
