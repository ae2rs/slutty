//! Watches the config file for changes with GCD vnode sources: no polling,
//! nothing runs until the file system reports a change.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use block2::RcBlock;
use dispatch2::{
    _dispatch_source_type_vnode, DispatchObject, DispatchQueue, DispatchRetained, DispatchSource,
    dispatch_source_type_t, dispatch_source_vnode_flags_t,
};

/// Calls back on the main queue when the file or its directory changes. The
/// file is watched directly for edits in place, and the directory for saves
/// that replace the file. Each change ends the watch: make a new one then,
/// so a replaced file is followed.
pub struct Watcher {
    sources: Vec<DispatchRetained<DispatchSource>>,
}

impl Watcher {
    pub fn new(path: &Path, on_change: impl Fn() + 'static) -> Self {
        let on_change = RcBlock::new(on_change);
        let file = dispatch_source_vnode_flags_t::DISPATCH_VNODE_WRITE.0
            | dispatch_source_vnode_flags_t::DISPATCH_VNODE_EXTEND.0
            | dispatch_source_vnode_flags_t::DISPATCH_VNODE_DELETE.0
            | dispatch_source_vnode_flags_t::DISPATCH_VNODE_RENAME.0
            | dispatch_source_vnode_flags_t::DISPATCH_VNODE_REVOKE.0;
        let dir = dispatch_source_vnode_flags_t::DISPATCH_VNODE_WRITE.0;
        let sources = [(Some(path), file), (path.parent(), dir)]
            .into_iter()
            .filter_map(|(path, mask)| source(path?, mask, &on_change))
            .collect();
        Self { sources }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        for source in &self.sources {
            source.cancel();
        }
    }
}

fn source(path: &Path, mask: u64, on_change: &RcBlock<dyn Fn()>) -> Option<DispatchRetained<DispatchSource>> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: a valid C string; O_EVTONLY opens for events only.
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_EVTONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    let kind: dispatch_source_type_t = (&raw const _dispatch_source_type_vnode).cast_mut();
    // SAFETY: a vnode source takes an open descriptor and vnode flags.
    let source = unsafe { DispatchSource::new(kind, fd as usize, mask as usize, Some(DispatchQueue::main())) };
    let close = RcBlock::new(move || {
        // SAFETY: the descriptor is ours, and the source is done with it.
        unsafe { libc::close(fd) };
    });
    // SAFETY: the source copies both blocks.
    unsafe {
        source.set_event_handler_with_block(RcBlock::as_ptr(on_change));
        source.set_cancel_handler_with_block(RcBlock::as_ptr(&close));
    }
    source.resume();
    Some(source)
}
