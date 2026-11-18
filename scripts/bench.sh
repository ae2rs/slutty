#!/bin/sh
# Compares slutty with WezTerm: vtebench throughput, idle CPU and wake-ups,
# and memory at launch and with a full scrollback. Writes raw results to
# docs/bench/<terminal>/ and prints a summary.
#
# Every run opens a window that takes focus: start it, then leave the Mac
# alone until it ends (about 10 minutes). Usage: scripts/bench.sh [slutty|wezterm]...
set -eu

cd "$(dirname "$0")/.."
root=$(pwd)
out=$root/docs/bench
work=$root/target/bench
vte=$work/vtebench
cols=100
rows=30
scrollback=100000

# Starts terminal $1 running `sh -c "$2"`. Sets $launcher to the process to
# wait for and $pid to the one that draws.
start() {
    case $1 in
    slutty)
        # An empty config directory: slutty's defaults.
        XDG_CONFIG_HOME=$work/config target/release/slutty -- sh -c "$2" &
        launcher=$!
        pid=$launcher
        ;;
    wezterm)
        # Defaults, with slutty's font size, window size and scrollback.
        wezterm -n --config font_size=16 --config initial_cols=$cols --config initial_rows=$rows \
            --config scrollback_lines=$scrollback start --always-new-process -- sh -c "$2" &
        launcher=$!
        sleep 2
        pid=$(pgrep -P "$launcher" wezterm-gui || echo "$launcher")
        ;;
    *)
        echo "unknown terminal: $1" >&2
        exit 2
        ;;
    esac
}

# Waits for file $1 to appear.
await() {
    while [ ! -e "$1" ]; do sleep 1; done
}

# Physical footprint and resident size of $pid, in one line.
memory() {
    footprint=$(footprint -p "$pid" | sed -n 's/.*Footprint: \(.*\) (.*/\1/p')
    echo "footprint $footprint, rss $(ps -o rss= -p "$pid" | tr -d ' ') KB"
}

throughput() {
    start "$1" "stty size > '$dir/size.txt'; vtebench -b '$vte/benchmarks' --dat '$dir/vtebench.dat'"
    wait "$launcher" || true
}

# Memory at launch, then CPU and idle wake-ups over 60 s at a still screen.
idle() {
    rm -f "$work/ready"
    start "$1" "touch '$work/ready'; exec sleep 90"
    await "$work/ready"
    sleep 5
    echo "launch: $(memory)" > "$dir/memory.txt"
    before=$(ps -o time= -p "$pid")
    top -l 4 -s 20 -pid "$pid" -stats pid,cpu,idlew,time > "$dir/idle.txt"
    echo "cpu time: $before -> $(ps -o time= -p "$pid")" >> "$dir/idle.txt"
    wait "$launcher" || true
}

# Memory once the scrollback is full.
scrollback() {
    rm -f "$work/ready"
    start "$1" "awk 'BEGIN { for (i = 1; i <= 2 * $scrollback; i++) printf \"%06d the quick brown fox jumps over the lazy dog 0123456789 abcdefghij\\n\", i }'; touch '$work/ready'; exec sleep 20"
    await "$work/ready"
    sleep 5
    echo "full scrollback: $(memory)" >> "$dir/memory.txt"
    wait "$launcher" || true
}

# Mean of each vtebench column, in ms.
means() {
    awk 'NR == 1 { for (i = 1; i <= NF; i++) name[i] = $i; n = NF; next }
        { for (i = 1; i <= NF; i++) if ($i != "_") { sum[i] += $i; count[i]++ } }
        END { for (i = 1; i <= n; i++) printf "%s %.2f\n", name[i], sum[i] / count[i] }' "$1"
}

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

terminals=${*:-slutty wezterm}
for terminal in $terminals; do
    dir=$out/$terminal
    mkdir -p "$dir"
    echo "$terminal: throughput"
    throughput "$terminal"
    echo "$terminal: idle"
    idle "$terminal"
    echo "$terminal: scrollback"
    scrollback "$terminal"
done

cat "$out/machine.txt"
for terminal in $terminals; do
    dir=$out/$terminal
    echo
    echo "## $terminal ($(cat "$dir/size.txt") rows, cols)"
    means "$dir/vtebench.dat"
    echo "pid cpu idle-wakeups time, every 20 s:"
    awk '$1 ~ /^[0-9]+$/' "$dir/idle.txt"
    tail -1 "$dir/idle.txt"
    cat "$dir/memory.txt"
done
