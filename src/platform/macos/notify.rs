//! Desktop notifications from programs (OSC 9, OSC 777), posted through
//! UserNotifications. A click brings the pane that asked to the front, and
//! with AeroSpace, its workspace too.
//!
//! UserNotifications needs a bundle, so notifications are off when slutty
//! runs as a bare binary, as from `cargo run`.

use std::cell::{Cell, OnceCell};
use std::path::Path;
use std::process::Command;

use block2::{DynBlock, RcBlock};
use dispatch2::{DispatchQoS, DispatchQueue, GlobalQueueIdentifier};
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, MainThreadMarker, define_class, msg_send};
use objc2_app_kit::NSApplication;
use objc2_foundation::{NSBundle, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

use super::state::{self, with_app};
use crate::mux::PaneId;
use crate::terminal::Notification;

/// Where Homebrew puts AeroSpace; an app started from the Dock has no PATH.
const AEROSPACE: [&str; 2] = ["/opt/homebrew/bin/aerospace", "/usr/local/bin/aerospace"];

define_class!(
    #[unsafe(super(NSObject))]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        /// Shows notifications while slutty is in front too, as WezTerm does.
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            handler: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            handler.call((UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            handler: &DynBlock<dyn Fn()>,
        ) {
            let id = response.notification().request().identifier().to_string();
            // The center may call from any thread; the mux lives on the main one.
            DispatchQueue::main().exec_async(move || {
                if let Some(pane) = id.split('.').next().and_then(|pane| pane.parse().ok()) {
                    reveal(pane);
                }
            });
            handler.call(());
        }
    }
);

thread_local! {
    /// The center holds its delegate weakly; this keeps it.
    static DELEGATE: OnceCell<Retained<Delegate>> = const { OnceCell::new() };
    /// Makes each notification's identifier unique.
    static POSTED: Cell<u64> = const { Cell::new(0) };
}

/// Asks to post notifications, once at launch. macOS asks the user the
/// first time.
pub fn start() {
    if NSBundle::mainBundle().bundleIdentifier().is_none() {
        log::info!("not running from slutty.app; desktop notifications are off");
        return;
    }
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let delegate: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(), init] };
    center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    DELEGATE.with(|cell| _ = cell.set(delegate));
    let done = RcBlock::new(|granted: Bool, _error: *mut NSError| {
        if !granted.as_bool() {
            log::warn!("desktop notifications are not allowed; System Settings > Notifications turns them on");
        }
    });
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
        &done,
    );
}

/// Posts a notification that brings `pane` to the front when clicked.
pub fn post(pane: PaneId, notification: &Notification) {
    if DELEGATE.with(|cell| cell.get().is_none()) {
        return;
    }
    let content = UNMutableNotificationContent::new();
    let title = if notification.title.is_empty() { "slutty" } else { &notification.title };
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(&notification.body));
    let n = POSTED.with(|posted| posted.replace(posted.get() + 1));
    let id = NSString::from_str(&format!("{pane}.{n}"));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
    let done = RcBlock::new(|error: *mut NSError| {
        // SAFETY: the center passes a valid error or null.
        if let Some(error) = unsafe { error.as_ref() } {
            log::warn!("posting a notification failed: {}", error.localizedDescription());
        }
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .addNotificationRequest_withCompletionHandler(&request, Some(&done));
}

/// Brings `pane` to the front: its window, tab and pane, then its AeroSpace
/// workspace.
fn reveal(pane: PaneId) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Some(window) = with_app(|app| app.mux.reveal(pane)).flatten() else { return };
    let Some(view) = state::view(window) else { return };
    view.tabs_changed();
    NSApplication::sharedApplication(mtm).activate();
    let Some(ns_window) = view.window() else { return };
    ns_window.makeKeyAndOrderFront(None);
    // AeroSpace hides windows on other workspaces; its window ids are the
    // window server's, which AppKit calls window numbers.
    let number = ns_window.windowNumber();
    if let Some(aerospace) = AEROSPACE.into_iter().find(|path| Path::new(path).exists()) {
        let queue = DispatchQueue::global_queue(GlobalQueueIdentifier::QualityOfService(DispatchQoS::UserInitiated));
        queue.exec_async(move || {
            let focused = Command::new(aerospace).args(["focus", "--window-id", &number.to_string()]).status();
            if let Err(err) = focused {
                log::warn!("aerospace focus failed: {err}");
            }
        });
    }
}
