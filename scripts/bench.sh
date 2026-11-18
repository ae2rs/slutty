#!/bin/sh
# Compares slutty with WezTerm: vtebench throughput, idle CPU and wake-ups,
# and memory at launch and with a full scrollback. Two setups:
# - defaults: both terminals as shipped, with the same font size, window size
#   and scrollback;
# - mine: both with the user's own config, same window size.
# Writes raw results to docs/bench/<setup>/<terminal>/ and prints a summary.
# With --screenshot, also captures docs/screenshot.png.
#
# Every run opens a window that takes focus: start it, then leave the Mac
# alone until it ends (about 20 minutes).
# Usage: scripts/bench.sh [--screenshot] [defaults|mine]...
set -eu

cd "$(dirname "$0")/.."
root=$(pwd)
out=$root/docs/bench
work=$root/target/bench
vte=$work/vtebench
cols=100
rows=30
scrollback=100000

# Starts terminal $1 in setup $setup running `sh -c "$2"`, and sets $launcher
# to the process it started.
start() {
    rm -f "$work/ready"
    case $1-$setup in
    slutty-defaults)
        # An empty config directory: slutty's defaults.
        XDG_CONFIG_HOME=$work/config target/release/slutty -- sh -c "$2" &
        ;;
    slutty-mine)
        target/release/slutty -- sh -c "$2" &
        ;;
    wezterm-defaults)
        wezterm -n --config font_size=16 --config initial_cols=$cols --config initial_rows=$rows \
            --config scrollback_lines=$scrollback start --always-new-process -- sh -c "$2" &
        ;;
    wezterm-mine)
        wezterm --config initial_cols=$cols --config initial_rows=$rows \
            start --always-new-process -- sh -c "$2" &
        ;;
    *)
        echo "unknown terminal or setup: $1 $setup" >&2
        exit 2
        ;;
    esac
    launcher=$!
}

# Waits up to 10 minutes for the started command to touch $work/ready, then
# sets $pid to the process that draws.
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
    pid=$(pgrep -P "$launcher" wezterm-gui || echo "$launcher")
}

# Ends the started terminal, whether or not it closed by itself.
stop() {
    kill "${pid:-$launcher}" "$launcher" 2> /dev/null || true
    wait "$launcher" 2> /dev/null || true
    pid=
    sleep 2
}

# Physical footprint and resident size of $pid, in one line.
memory() {
    footprint=$(footprint -p "$pid" | sed -n 's/.*Footprint: \(.*\) (.*/\1/p')
    echo "footprint $footprint, rss $(ps -o rss= -p "$pid" | tr -d ' ') KB"
}

throughput() {
    start "$1" "stty size > '$dir/size.txt'; vtebench -b '$vte/benchmarks' --dat '$dir/vtebench.dat'; touch '$work/ready'"
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
# The split and nvim need synthetic keys; without Accessibility access the
# shot has one pane.
screenshot() {
    setup=mine
    start slutty "clear; git log --graph --oneline --decorate --color -24; touch '$work/ready'; exec zsh -i"
    await
    sleep 2
    keys() { osascript -e "tell application \"System Events\" to $1" 2> /dev/null || true; }
    keys 'keystroke "s" using control down'
    keys 'keystroke "v"'
    sleep 1
    keys 'keystroke "nvim src/main.rs"'
    keys 'key code 36'
    sleep 3
    window=$(aerospace list-windows --all --format '%{window-id} %{app-pid}' | awk -v pid="$pid" '$2 == pid { print $1; exit }')
    if [ -n "$window" ]; then
        screencapture -x -o -l "$window" "$root/docs/screenshot.png"
        echo "captured docs/screenshot.png"
    else
        echo "no slutty window found for the screenshot" >&2
    fi
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
            echo "$setup $terminal: $run"
            $run $terminal
        done
    done
done
if $shot; then
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
