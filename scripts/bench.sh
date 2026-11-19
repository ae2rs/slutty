#!/bin/sh
# Compares slutty with WezTerm: vtebench throughput, idle CPU and wake-ups,
# and memory at launch and with a full scrollback. Two setups:
# - defaults: both terminals as shipped, with the same font size and
#   scrollback;
# - mine: both with the user's own config.
# Each window runs alone and in front on an empty AeroSpace workspace of the
# main monitor, so it fills the same screen every time. Writes raw results to
# docs/bench/<setup>/<terminal>/ and prints a summary.
#
# slutty runs on a socket of its own under target/bench, so it never
# attaches to the server of the slutty in use, and its server is counted
# with the app: memory and CPU are app plus server.
#
# Every run raises a window: start it, then leave the Mac alone until it ends
# (about 15 minutes). It stops if the keyboard or mouse is used.
# Usage: scripts/bench.sh [defaults|mine]...
set -eu

cd "$(dirname "$0")/.."
root=$(pwd)
out=$root/docs/bench
work=$root/target/bench
vte=$work/vtebench
scrollback=100000
socket=$work/mux.sock

# Starts terminal $1 in setup $setup running `sh -c "$2"` once the window is
# placed, and sets $launcher to the process it started. The command first
# writes the grid to $work/size once it stops changing.
start() {
    rm -f "$work/ready" "$work/go" "$work/size"
    # New windows open on the focused workspace: no move across monitors.
    aerospace workspace "$bench_ws"
    command="while [ ! -e '$work/go' ]; do sleep 0.2; done
        size=; until [ \"\$size\" = \"\$(stty size)\" ]; do size=\$(stty size); sleep 1; done
        echo \"\$size\" > '$work/size'; $2"
    case $1-$setup in
    slutty-defaults)
        # An empty config directory: slutty's defaults.
        XDG_CONFIG_HOME=$work/config target/release/slutty --socket "$socket" -- sh -c "$command" &
        ;;
    slutty-mine)
        target/release/slutty --socket "$socket" -- sh -c "$command" &
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
    # slutty's panes run in its server, started by the app.
    server=$(pgrep -f "slutty server --daemon --socket $socket" || true)
}

# Moves the started terminal's window alone onto the empty workspace $bench_ws,
# where it fills the screen instead of being tiled beside others, brings it to
# the front, then lets its command run. Sets $pid to the process that draws.
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
    front "$pid"
    sleep 2
    touch "$work/go"
}

# Makes process $1 the active app and checks that macOS agrees: AeroSpace's
# focus does not activate an app a script started. Never send keys instead.
front() {
    osascript -e "tell application \"System Events\" to set frontmost of (first process whose unix id is $1) to true" > /dev/null
    sleep 1
    if [ "$(lsappinfo info -only pid "$(lsappinfo front)")" != "\"pid\"=$1" ]; then
        echo "could not bring $1 to the front" >&2
        stop
        exit 1
    fi
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

# Ends the started terminal, whether or not it closed by itself, and
# slutty's server, which outlives the app.
stop() {
    kill "${pid:-$launcher}" "$launcher" 2> /dev/null || true
    wait "$launcher" 2> /dev/null || true
    pkill -f "slutty server --daemon --socket $socket" 2> /dev/null || true
    pid=
    server=
    sleep 2
}

# Stops the batch if someone used the keyboard or mouse in the last minute:
# each run raises a window.
away() {
    idle=$(ioreg -c IOHIDSystem | awk '/HIDIdleTime/ { print int($NF / 1e9); exit }')
    if [ "$idle" -lt 60 ]; then
        echo "input $idle s ago: someone is at the Mac, stopping" >&2
        exit 1
    fi
}

# Physical footprint and resident size of $pid and of slutty's server, one
# line each.
memory() {
    for p in $pid ${server:-}; do
        footprint=$(footprint -p "$p" | sed -n 's/.*Footprint: \(.*\) (.*/\1/p')
        echo "pid $p: footprint $footprint, rss $(ps -o rss= -p "$p" | tr -d ' ') KB"
    done
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
    echo "launch:" > "$dir/memory.txt"
    memory >> "$dir/memory.txt"
    pids=$pid${server:+,$server}
    before=$(ps -o time= -p "$pids" | tr '\n' ' ')
    top -l 4 -s 20 -pid "$pid" ${server:+-pid "$server"} -stats pid,cpu,idlew,time > "$dir/idle.txt"
    echo "cpu time: $before-> $(ps -o time= -p "$pids" | tr '\n' ' ')" >> "$dir/idle.txt"
    stop
}

# Memory once the scrollback is full.
scrollback() {
    start "$1" "awk 'BEGIN { for (i = 1; i <= 2 * $scrollback; i++) printf \"%06d the quick brown fox jumps over the lazy dog 0123456789 abcdefghij\\n\", i }'; touch '$work/ready'; exec sleep 600"
    await
    sleep 5
    echo "full scrollback:" >> "$dir/memory.txt"
    memory >> "$dir/memory.txt"
    stop
}

# Mean of each vtebench column, in ms.
means() {
    awk 'NR == 1 { for (i = 1; i <= NF; i++) name[i] = $i; n = NF; next }
        { for (i = 1; i <= NF; i++) if ($i != "_") { sum[i] += $i; count[i]++ } }
        END { for (i = 1; i <= n; i++) printf "%s %.2f\n", name[i], sum[i] / count[i] }' "$1"
}

setups=${*:-defaults mine}

[ -d "$vte" ] || git clone --quiet --depth 1 --branch v0.3.1 https://github.com/alacritty/vtebench "$vte"
command -v vtebench > /dev/null || cargo install --locked vtebench
cargo build --release --locked
mkdir -p "$work/config" "$out"
home_ws=$(aerospace list-workspaces --focused)
home_app=$(lsappinfo info -only pid "$(lsappinfo front)" | tr -dc 0-9)
bench_ws=$(for ws in $(aerospace list-workspaces --all); do
    [ -n "$(aerospace list-windows --workspace "$ws")" ] || { echo "$ws"; break; }
done)
[ -n "$bench_ws" ] || { echo "no empty AeroSpace workspace" >&2; exit 1; }
aerospace move-workspace-to-monitor --workspace "$bench_ws" main
trap 'aerospace workspace "$home_ws"; osascript -e "tell application \"System Events\" to set frontmost of (first process whose unix id is $home_app) to true" > /dev/null || true' EXIT

# Before the results below change: they are tracked and would read as dirty.
version=$(git describe --always --dirty)
{
    echo "machine: $(sysctl -n hw.model), $(sysctl -n machdep.cpu.brand_string), $(sysctl -n hw.memsize | awk '{ print $1 / 2^30 }') GB"
    echo "macOS: $(sw_vers -productVersion)"
    echo "slutty: $version"
    echo "wezterm: $(wezterm --version)"
    echo "vtebench: $(vtebench --version)"
} > "$out/machine.txt"

for setup in $setups; do
    cols=
    for terminal in slutty wezterm; do
        dir=$out/$setup/$terminal
        mkdir -p "$dir"
        : > "$dir/size.txt"
        for run in throughput idle scrollback; do
            away
            echo "$setup $terminal: $run"
            $run $terminal
            read -r rows width < "$work/size"
            echo "$run $rows $width" >> "$dir/size.txt"
            # WezTerm's tab bar takes rows, but the width must match.
            : "${cols:=$width}"
            if [ "$width" != "$cols" ]; then
                echo "$setup $terminal $run: $width columns, others had $cols" >&2
                exit 1
            fi
        done
    done
done

cat "$out/machine.txt"
for setup in $setups; do
    for terminal in slutty wezterm; do
        dir=$out/$setup/$terminal
        echo
        echo "## $setup: $terminal ($(awk '{ print $2 "×" $3 }' "$dir/size.txt" | sort -u | tr '\n' ' ')cells)"
        means "$dir/vtebench.dat"
        echo "pid cpu idle-wakeups time, every 20 s:"
        awk '$1 ~ /^[0-9]+$/' "$dir/idle.txt"
        tail -1 "$dir/idle.txt"
        cat "$dir/memory.txt"
    done
done
