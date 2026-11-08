#!/usr/bin/env bash
# Measure the launcher's memory while hidden, in a throwaway config.
#
#   scripts/rss.sh [binary] [-- query ...]
#
# Starts `kairos --background`, shows it (once per query, typed in with `--query`), hides it
# and reports VmRSS / RssAnon / RssFile / PSS (KiB) at each step. Fails when the RSS once
# hidden (after the launcher handed memory back, 3 s after hiding) is over the budget
# (KAIROS_RSS_BUDGET_KB, default 20 MiB). Plugin processes are not counted. Needs a desktop
# session (Wayland or X11). KAIROS_RSS_HOOK=<program> runs it with the pid after hiding.
set -euo pipefail

bin=target/release/kairos
if [[ $# -gt 0 && $1 != -- ]]; then bin=$1; shift; fi
[[ ${1:-} == -- ]] && shift
queries=("$@")
budget=${KAIROS_RSS_BUDGET_KB:-20480}

# A short runtime folder: the control socket path must stay under ~100 bytes.
rt=$(mktemp -d /tmp/kairos-rss.XXXX)
home=$(mktemp -d)
cleanup() {
    [[ -n ${pid:-} ]] && kill "$pid" 2>/dev/null || true
    rm -rf "$rt" "$home"
}
trap cleanup EXIT
chmod 700 "$rt"
# The Wayland socket name is relative to the real runtime folder, which is replaced below.
wl=${WAYLAND_DISPLAY:-}
[[ -z $wl && -S ${XDG_RUNTIME_DIR:-/run/user/$UID}/wayland-0 ]] && wl=wayland-0
[[ -n $wl && $wl != /* ]] && wl=${XDG_RUNTIME_DIR:-/run/user/$UID}/$wl
[[ -n $wl ]] && export WAYLAND_DISPLAY=$wl
export XDG_RUNTIME_DIR=$rt KAIROS_HOME=$home
# Plugins shipped next to a dist build are used; with a target/ build point KAIROS_PLUGINS at them.
[[ -z ${KAIROS_PLUGINS:-} && -d dist/plugins ]] && export KAIROS_PLUGINS=$PWD/dist/plugins

"$bin" --background >"$home/log.txt" 2>&1 &
pid=$!

kb() { awk -v k="$1:" '$1 == k { print $2 }' "/proc/$pid/status"; }
fail=0
report() {
    local label=$1 hidden=$2 rss
    if [[ ! -r /proc/$pid/status ]]; then
        echo "the launcher exited; its log:" >&2
        cat "$home/log.txt" >&2
        exit 2
    fi
    rss=$(kb VmRSS)
    local pss
    pss=$(awk '$1 == "Pss:" { print $2 }' "/proc/$pid/smaps_rollup")
    printf '%-26s VmRSS %6s  anon %6s  file %6s  PSS %6s KiB\n' "$label" "$rss" "$(kb RssAnon)" "$(kb RssFile)" "$pss"
    if [[ $hidden == 1 && $rss -gt $budget ]]; then
        echo "  over budget ($budget KiB)"
        fail=1
    fi
}

sleep 5
report "started hidden" 1
if [[ ${#queries[@]} -eq 0 ]]; then
    "$bin" --show
    sleep 2
    report "shown" 0
else
    for q in "${queries[@]}"; do
        "$bin" --query "$q"
        sleep 3
        report "shown: $q" 0
    done
fi
"$bin" --hide
sleep 1
report "hidden 1 s" 0
sleep 4
# The launcher hands memory back 3 s after hiding; from then on the budget applies.
report "hidden 5 s" 1
[[ -n ${KAIROS_RSS_HOOK:-} ]] && "$KAIROS_RSS_HOOK" "$pid"
sleep 35
report "hidden 40 s" 1
exit $fail
