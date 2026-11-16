//! Stats from two readings of the counters, and their text at fixed widths.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::counters::{self, Counters};
use crate::config::Stat;

/// Readings further apart than this give no rates: the window was away, and
/// an average over that long would mislead.
const MAX_GAP: Duration = Duration::from_secs(10);

/// What the tab bar shows. `None` reads `--`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    /// Bytes in use.
    pub ram: Option<u64>,
    /// Whole percent of all cores busy.
    pub cpu: Option<u8>,
    /// Bytes a second received and sent.
    pub net: Option<(f64, f64)>,
    /// Whole percent of the data volume in use.
    pub disk: Option<u8>,
}

impl Stats {
    /// The stats at `after`, with rates since `before` when it is recent.
    fn between(before: Option<&(Instant, Counters)>, (now, after): (Instant, &Counters)) -> Self {
        let before = before.filter(|(at, _)| now.saturating_duration_since(*at) <= MAX_GAP);
        let cpu = before.and_then(|(_, b)| {
            let (b, a) = (b.cpu?, after.cpu?);
            let total = a.total.checked_sub(b.total).filter(|&t| t > 0)?;
            let busy = a.busy.saturating_sub(b.busy).min(total);
            u8::try_from((busy * 100 + total / 2) / total).ok()
        });
        let net = before.and_then(|(at, b)| {
            let secs = now.saturating_duration_since(*at).as_secs_f64();
            let (rx, tx) = net_delta(b.net.as_ref()?, after.net.as_ref()?);
            (secs > 0.0).then(|| (rx as f64 / secs, tx as f64 / secs))
        });
        Self { ram: after.ram, cpu, net, disk: after.disk }
    }
}

/// Bytes moved between two readings, over the interfaces in both.
fn net_delta(before: &BTreeMap<String, (u32, u32)>, after: &BTreeMap<String, (u32, u32)>) -> (u64, u64) {
    after.iter().filter_map(|(name, &(rx, tx))| before.get(name).map(|&(brx, btx)| (rx, tx, brx, btx))).fold(
        (0, 0),
        |(sum_rx, sum_tx), (rx, tx, brx, btx)| {
            (sum_rx + u64::from(rx.wrapping_sub(brx)), sum_tx + u64::from(tx.wrapping_sub(btx)))
        },
    )
}

/// Reads the counters when asked and keeps the stats they give.
pub struct Monitor {
    host: libc::mach_port_t,
    last: Option<(Instant, Counters)>,
    stats: Stats,
}

impl Monitor {
    pub fn new() -> Self {
        Self { host: counters::host(), last: None, stats: Stats::default() }
    }

    /// Reads the counters again.
    pub fn refresh(&mut self, now: Instant) {
        let counters = counters::read(self.host);
        self.stats = Stats::between(self.last.as_ref(), (now, &counters));
        self.last = Some((now, counters));
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }
}

/// How a piece of the status text is colored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ink {
    Text,
    /// The received arrow.
    Down,
    /// The sent arrow.
    Up,
}

/// `items` as text pieces, separated by ` | ` as WezTerm's tabline lays
/// them out: `12.34 GB | 7% | ↓12K ↑900B | 61%`. The width varies with the
/// values, up to [`width`].
pub fn segments(items: &[Stat], stats: &Stats) -> Vec<(String, Ink)> {
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push((" | ".to_owned(), Ink::Text));
        }
        match item {
            Stat::Ram => out.push((ram(stats.ram), Ink::Text)),
            Stat::Cpu => out.push((percent(stats.cpu), Ink::Text)),
            Stat::Net => {
                let (rx, tx) = stats.net.map_or((None, None), |(rx, tx)| (Some(rx), Some(tx)));
                out.extend([
                    ("↓".to_owned(), Ink::Down),
                    (format!("{} ", rate(rx)), Ink::Text),
                    ("↑".to_owned(), Ink::Up),
                    (rate(tx), Ink::Text),
                ]);
            }
            Stat::Disk => out.push((percent(stats.disk), Ink::Text)),
        }
    }
    out
}

/// The most cells `items` can take, so the tabs can stop short of them
/// whatever the values.
pub fn width(items: &[Stat]) -> usize {
    let widest = Stats { ram: Some(u64::MAX), cpu: Some(100), net: Some((f64::MAX, f64::MAX)), disk: Some(100) };
    segments(items, &widest).iter().map(|(text, _)| text.chars().count()).sum()
}

/// `12.34 GB`, at most eight cells.
fn ram(bytes: Option<u64>) -> String {
    let Some(b) = bytes else { return "-- GB".to_owned() };
    let gb = (b as f64 / f64::from(1 << 30)).min(999.9);
    if gb < 99.995 { format!("{gb:.2} GB") } else { format!("{gb:.1} GB") }
}

/// `7%`, at most four cells.
fn percent(value: Option<u8>) -> String {
    format!("{}%", value.map_or_else(|| "--".to_owned(), |v| v.min(100).to_string()))
}

/// A rate in at most six cells: `999B`, `12K`, `1.5M`, `2.0G`. Each unit
/// stops where rounding would take it to four digits.
fn rate(bytes: Option<f64>) -> String {
    let Some(b) = bytes else { return "--".to_owned() };
    let (k, m, g) = (b / 1024.0, b / (1024.0 * 1024.0), b / (1024.0 * 1024.0 * 1024.0));
    if b < 1000.0 {
        format!("{}B", b.max(0.0) as u64)
    } else if k < 999.5 {
        format!("{k:.0}K")
    } else if m < 999.95 {
        format!("{m:.1}M")
    } else {
        format!("{:.1}G", g.min(999.9))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::counters::Ticks;

    const ALL: [Stat; 4] = [Stat::Ram, Stat::Cpu, Stat::Net, Stat::Disk];

    fn text(items: &[Stat], stats: &Stats) -> String {
        segments(items, stats).into_iter().map(|(text, _)| text).collect()
    }

    #[test]
    fn reads_like_wezterm() {
        let stats = Stats { ram: Some(13_249_974_272), cpu: Some(7), net: Some((12_800.0, 900.0)), disk: Some(61) };
        assert_eq!(text(&ALL, &stats), "12.34 GB | 7% | ↓12K ↑900B | 61%");
        assert_eq!(text(&[Stat::Cpu], &Stats::default()), "--%");
    }

    #[test]
    fn no_values_outgrow_the_width() {
        let extremes = [
            Stats::default(),
            Stats { ram: Some(0), cpu: Some(0), net: Some((0.0, 0.0)), disk: Some(0) },
            Stats { ram: Some(u64::MAX), cpu: Some(100), net: Some((1e15, 999.9)), disk: Some(100) },
            Stats { ram: Some(107_000_000_000), cpu: Some(55), net: Some((1_048_000.0, 1_023_999.0)), disk: Some(9) },
            Stats {
                ram: Some(107_369_000_000),
                cpu: Some(99),
                net: Some((1_048_575_000.0, 1_023_487.0)),
                disk: Some(99),
            },
        ];
        for stats in extremes {
            assert!(text(&ALL, &stats).chars().count() <= width(&ALL), "{stats:?}");
        }
        assert_eq!(width(&ALL), 8 + 4 + 15 + 4 + 3 * 3);
    }

    #[test]
    fn rates_pick_a_unit() {
        let r = |b: f64| rate(Some(b));
        assert_eq!(
            (r(0.0), r(999.0), r(12_800.0), r(1_023_600.0), r(1_572_864.0), r(3e9)),
            ("0B".into(), "999B".into(), "12K".into(), "1.0M".into(), "1.5M".into(), "2.8G".into())
        );
    }

    fn reading(busy: u64, total: u64, rx: u32, tx: u32) -> Counters {
        Counters {
            cpu: Some(Ticks { busy, total }),
            ram: Some(1 << 30),
            net: Some(BTreeMap::from([("en0".to_owned(), (rx, tx))])),
            disk: Some(50),
        }
    }

    #[test]
    fn rates_come_from_the_change_since_a_recent_reading() {
        let t0 = Instant::now();
        let before = (t0, reading(100, 1_000, u32::MAX - 999, 0));
        let stats = Stats::between(Some(&before), (t0 + Duration::from_secs(2), &reading(400, 2_000, 1_000, 4_000)));
        assert_eq!(stats.cpu, Some(30));
        assert_eq!(stats.net, Some((1_000.0, 2_000.0)), "the received count wrapped");
        assert_eq!((stats.ram, stats.disk), (Some(1 << 30), Some(50)));
    }

    #[test]
    fn an_old_or_missing_reading_gives_no_rates() {
        let t0 = Instant::now();
        let before = (t0, reading(100, 1_000, 0, 0));
        let late = Stats::between(Some(&before), (t0 + Duration::from_secs(60), &reading(400, 2_000, 9, 9)));
        assert_eq!((late.cpu, late.net, late.disk), (None, None, Some(50)));
        assert_eq!(Stats::between(None, (t0, &reading(1, 2, 3, 4))).cpu, None);
    }
}
