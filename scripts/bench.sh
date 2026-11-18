#!/bin/sh
# Compares slutty with WezTerm: vtebench throughput, idle CPU and wake-ups,
# and memory at launch and with a full scrollback. Two setups:
# - defaults: both terminals as shipped, with the same font size and
#   scrollback;
# - mine: both with the user's own config.
# Each window runs alone on an empty AeroSpace workspace, so it fills the
# screen and has focus. Writes raw results to docs/bench/<setup>/<terminal>/
# and prints a summary. With --screenshot, also captures docs/screenshot.png.
#
# Every run takes focus: start it, then leave the Mac alone until it ends
# (about 20 minutes). It stops if the keyboard or mouse is used.
# Usage: scripts/bench.sh [--screenshot] [defaults|mine]...
set -eu

cd "$(dirname "$0")/.."
root=$(pwd)
out=$root/docs/bench
work=$root/target/bench
vte=$work/vtebench
scrollback=100000

# Starts terminal $1 in setup $setup running `sh -c "$2"` once the window is
# placed, and sets $launcher to the process it started.
start() {
    rm -f "$work/ready" "$work/go"
    command="while [ ! -e '$work/go' ]; do sleep 0.2; done; stty size > '$dir/size.txt'; $2"
    case $1-$setup in
    slutty-defaults)
        # An empty config directory: slutty's defaults.
        XDG_CONFIG_HOME=$work/config target/release/slutty -- sh -c "$command" &
        ;;
    slutty-mine)
        target/release/slutty -- sh -c "$command" &
        ;;
    wezterm-defaults)
        wezterm -n --config font_size=16 --config scrollback_lines=$scrollback \
            start --always-new-process -- sh -c "$command" &
        ;;
    wezterm-mine)
        wezterm start --always-new-process -- sh -c "$command" &
        ;;
    *)
        echo "unknown terminal or setup: $1 $setup" >&2
        exit 2
        ;;
    esac
    launcher=$!
    place
}

# Moves the started terminal's window alone onto the empty workspace $bench_ws
# and focuses it, then lets its command run. A window opened from a script is
# not made key, and AeroSpace would tile it beside others. Sets $pid to the
# process that draws.
place() {
    i=0
    while :; do
        pid=$(pgrep -P "$launcher" wezterm-gui || echo "$launcher")
        window=$(aerospace list-windows --all --format '%{window-id} %{app-pid}' | awk -v pid="$pid" '$2 == pid { print $1; exit }')
        [ -z "$window" ] || break
        i=$((i + 1))
        if [ $i -gt 60 ]; then
            echo "no window for $pid" >&2
            stop
            exit 1
        fi
        sleep 0.5
    done
    aerospace move-node-to-workspace --window-id "$window" "$bench_ws"
    aerospace workspace "$bench_ws"
    aerospace focus --window-id "$window"
    sleep 2
    touch "$work/go"
}

# Waits up to 10 minutes for the command to touch $work/ready.
await() {
    i=0
    until [ -e "$work/ready" ]; do
        i=$((i + 1))
        if [ $i -gt 600 ]; then
            echo "timed out waiting for $work/ready" >&2
            stop
            exit 1
        fi
        sleep 1
    done
}

# Ends the started terminal, whether or not it closed by itself.
stop() {
    kill "${pid:-$launcher}" "$launcher" 2> /dev/null || true
    wait "$launcher" 2> /dev/null || true
    pid=
    sleep 2
}

# Stops the batch if someone used the keyboard or mouse in the last minute:
# each run takes focus.
away() {
    idle=$(ioreg -c IOHIDSystem | awk '/HIDIdleTime/ { print int($NF / 1e9); exit }')
    if [ "$idle" -lt 60 ]; then
        echo "input $idle s ago: someone is at the Mac, stopping" >&2
        exit 1
    fi
}

# Physical footprint and resident size of $pid, in one line.
memory() {
    footprint=$(footprint -p "$pid" | sed -n 's/.*Footprint: \(.*\) (.*/\1/p')
    echo "footprint $footprint, rss $(ps -o rss= -p "$pid" | tr -d ' ') KB"
}

throughput() {
    start "$1" "vtebench -b '$vte/benchmarks' --dat '$dir/vtebench.dat'; touch '$work/ready'"
    await
    stop
}

# Memory at launch, then CPU and idle wake-ups over 60 s at a still screen.
idle() {
    start "$1" "touch '$work/ready'; exec sleep 600"
    await
    sleep 5
    echo "launch: $(memory)" > "$dir/memory.txt"
    before=$(ps -o time= -p "$pid")
    top -l 4 -s 20 -pid "$pid" -stats pid,cpu,idlew,time > "$dir/idle.txt"
    echo "cpu time: $before -> $(ps -o time= -p "$pid")" >> "$dir/idle.txt"
    stop
}

# Memory once the scrollback is full.
scrollback() {
    start "$1" "awk 'BEGIN { for (i = 1; i <= 2 * $scrollback; i++) printf \"%06d the quick brown fox jumps over the lazy dog 0123456789 abcdefghij\\n\", i }'; touch '$work/ready'; exec sleep 600"
    await
    sleep 5
    echo "full scrollback: $(memory)" >> "$dir/memory.txt"
    stop
}

# slutty with the user's config, a git log on the left and nvim on the right.
# The split and nvim are typed with synthetic keys, only while slutty has
# focus; without Accessibility access the shot has one pane.
screenshot() {
    setup=mine
    dir=$work
    start slutty "clear; git log --graph --oneline --decorate --color -24; touch '$work/ready'; exec zsh -i"
    await
    sleep 2
    if [ "$(aerospace list-windows --focused --format '%{app-pid}')" = "$pid" ]; then
        keys() { osascript -e "tell application \"System Events\" to $1" 2> /dev/null || true; }
        keys 'keystroke "s" using control down'
        keys 'keystroke "v"'
        sleep 1
        keys 'keystroke "nvim src/main.rs"'
        keys 'key code 36'
        sleep 3
    else
        echo "slutty lost focus; the screenshot has one pane" >&2
    fi
    screencapture -x -o -l "$window" "$root/docs/screenshot.png"
    echo "captured docs/screenshot.png"
    stop
}

# Mean of each vtebench column, in ms.
means() {
    awk 'NR == 1 { for (i = 1; i <= NF; i++) name[i] = $i; n = NF; next }
        { for (i = 1; i <= NF; i++) if ($i != "_") { sum[i] += $i; count[i]++ } }
        END { for (i = 1; i <= n; i++) printf "%s %.2f\n", name[i], sum[i] / count[i] }' "$1"
}

shot=false
if [ "${1:-}" = --screenshot ]; then
    shot=true
    shift
fi
setups=${*:-defaults mine}

[ -d "$vte" ] || git clone --quiet --depth 1 --branch v0.3.1 https://github.com/alacritty/vtebench "$vte"
command -v vtebench > /dev/null || cargo install --locked vtebench
cargo build --release --locked
mkdir -p "$work/config" "$out"
home_ws=$(aerospace list-workspaces --focused)
bench_ws=$(for ws in $(aerospace list-workspaces --all); do
    [ -n "$(aerospace list-windows --workspace "$ws")" ] || { echo "$ws"; break; }
done)
[ -n "$bench_ws" ] || { echo "no empty AeroSpace workspace" >&2; exit 1; }
trap 'aerospace workspace "$home_ws"' EXIT

{
    echo "machine: $(sysctl -n hw.model), $(sysctl -n machdep.cpu.brand_string), $(sysctl -n hw.memsize | awk '{ print $1 / 2^30 }') GB"
    echo "macOS: $(sw_vers -productVersion)"
    echo "slutty: $(git describe --always --dirty)"
    echo "wezterm: $(wezterm --version)"
    echo "vtebench: $(vtebench --version)"
} > "$out/machine.txt"

for setup in $setups; do
    for terminal in slutty wezterm; do
        dir=$out/$setup/$terminal
        mkdir -p "$dir"
        for run in throughput idle scrollback; do
            away
            echo "$setup $terminal: $run"
            $run $terminal
        done
    done
done
if $shot; then
    away
    screenshot
fi

cat "$out/machine.txt"
for setup in $setups; do
    for terminal in slutty wezterm; do
        dir=$out/$setup/$terminal
        echo
        echo "## $setup: $terminal ($(cat "$dir/size.txt") rows, cols)"
        means "$dir/vtebench.dat"
        echo "pid cpu idle-wakeups time, every 20 s:"
        awk '$1 ~ /^[0-9]+$/' "$dir/idle.txt"
        tail -1 "$dir/idle.txt"
        cat "$dir/memory.txt"
    done
done
