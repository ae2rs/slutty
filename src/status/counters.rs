//! Raw counters from the kernel, read with system calls: no process is
//! spawned. Each part is `None` when its call fails.

use std::collections::BTreeMap;
use std::ffi::CStr;
use std::mem;

/// One reading of every counter.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Counters {
    /// Busy and total CPU ticks over every core since boot.
    pub cpu: Option<Ticks>,
    /// Bytes in use, as Activity Monitor's "Memory Used": app, wired and
    /// compressed memory.
    pub ram: Option<u64>,
    /// Bytes received and sent by each Ethernet or Wi-Fi interface (`en*`).
    /// The kernel keeps 32-bit counts here, which wrap.
    pub net: Option<BTreeMap<String, (u32, u32)>>,
    /// Whole percent of the data volume in use, rounded up as `df` does.
    pub disk: Option<u8>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ticks {
    pub busy: u64,
    pub total: u64,
}

// libc deprecates its binding for the mach2 crate; one declaration is less.
unsafe extern "C" {
    safe fn mach_host_self() -> libc::mach_port_t;
}

/// The port to read host counters through. Each call adds a reference to it,
/// so keep the one returned.
pub fn host() -> libc::mach_port_t {
    mach_host_self()
}

/// Reads the counters through `host`.
pub fn read(host: libc::mach_port_t) -> Counters {
    Counters { cpu: cpu(host), ram: ram(host), net: net(), disk: disk() }
}

fn cpu(host: libc::mach_port_t) -> Option<Ticks> {
    // SAFETY: plain data, valid when zeroed.
    let mut info: libc::host_cpu_load_info = unsafe { mem::zeroed() };
    let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
    // SAFETY: `info` holds `count` integers.
    let status =
        unsafe { libc::host_statistics(host, libc::HOST_CPU_LOAD_INFO, (&raw mut info).cast(), &raw mut count) };
    (status == libc::KERN_SUCCESS).then(|| {
        let ticks = info.cpu_ticks.map(u64::from);
        let idle = ticks[libc::CPU_STATE_IDLE as usize];
        let total: u64 = ticks.iter().sum();
        Ticks { busy: total - idle, total }
    })
}

fn ram(host: libc::mach_port_t) -> Option<u64> {
    // SAFETY: plain data, valid when zeroed.
    let mut info: libc::vm_statistics64 = unsafe { mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: `info` holds `count` integers; the kernel fills what it knows.
    let status = unsafe { libc::host_statistics64(host, libc::HOST_VM_INFO64, (&raw mut info).cast(), &raw mut count) };
    // SAFETY: no preconditions.
    let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
    (status == libc::KERN_SUCCESS).then(|| {
        let app = u64::from(info.internal_page_count.saturating_sub(info.purgeable_count));
        (app + u64::from(info.wire_count) + u64::from(info.compressor_page_count)) * page
    })
}

fn net() -> Option<BTreeMap<String, (u32, u32)>> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: fills `list`, freed below.
    if unsafe { libc::getifaddrs(&raw mut list) } != 0 {
        return None;
    }
    let mut bytes = BTreeMap::new();
    let mut entry = list;
    while !entry.is_null() {
        // SAFETY: a node of the list getifaddrs made, alive until freed.
        let ifa = unsafe { &*entry };
        entry = ifa.ifa_next;
        // SAFETY: a non-null address of the node.
        let link = !ifa.ifa_addr.is_null() && i32::from(unsafe { (*ifa.ifa_addr).sa_family }) == libc::AF_LINK;
        if !link || ifa.ifa_data.is_null() {
            continue;
        }
        // SAFETY: a NUL-terminated name.
        let name = unsafe { CStr::from_ptr(ifa.ifa_name) }.to_string_lossy();
        if name.starts_with("en") {
            // SAFETY: for a link address, `ifa_data` points to its `if_data`.
            let data = unsafe { &*ifa.ifa_data.cast::<libc::if_data>() };
            bytes.insert(name.into_owned(), (data.ifi_ibytes, data.ifi_obytes));
        }
    }
    // SAFETY: the list getifaddrs made, no longer read.
    unsafe { libc::freeifaddrs(list) };
    Some(bytes)
}

fn disk() -> Option<u8> {
    // The data volume holds everything but the sealed system.
    [c"/System/Volumes/Data", c"/"].into_iter().find_map(|path| {
        // SAFETY: plain data, valid when zeroed.
        let mut fs: libc::statfs = unsafe { mem::zeroed() };
        // SAFETY: a NUL-terminated path and a buffer for the result.
        if unsafe { libc::statfs(path.as_ptr(), &raw mut fs) } != 0 {
            return None;
        }
        let used = fs.f_blocks.saturating_sub(fs.f_bfree);
        let usable = used + fs.f_bavail;
        (usable > 0).then(|| u8::try_from((used * 100).div_ceil(usable)).unwrap_or(100))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_plausible_counters() {
        let c = read(host());
        let cpu = c.cpu.expect("cpu ticks");
        assert!(cpu.total > 0 && cpu.busy <= cpu.total);
        let ram = c.ram.expect("ram");
        assert!(ram > 100 << 20, "more than 100 MB in use: {ram}");
        assert!(c.net.is_some());
        assert!(c.disk.is_some_and(|d| d <= 100));
    }
}
