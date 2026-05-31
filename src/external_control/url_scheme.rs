//! macOS `muninn://` custom URL scheme handler.
//!
//! Uses `NSAppleEventManager` (via raw `objc2` runtime messaging so we don't
//! disturb the existing `objc2-foundation` 0.2 dependency) to receive the
//! `GetURL` Apple Event that LaunchServices dispatches when a `muninn://` link
//! is opened. The handler is only effective for the packaged `.app` that
//! registers the scheme through `CFBundleURLTypes`.

use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{class, define_class, msg_send, sel, AnyThread};
use tao::event_loop::EventLoopProxy;
use tracing::{info, warn};

use super::parse_url_action;
use crate::logging::TARGET_RUNTIME;
use crate::runtime_tray::{send_user_event, UserEvent};

// FourCharCode constants from the Apple Event Manager headers.
const KEY_DIRECT_OBJECT: u32 = 0x2D2D_2D2D; // '----'
const INTERNET_EVENT_CLASS: u32 = 0x4755_524C; // 'GURL'
const AE_GET_URL: u32 = 0x4755_524C; // 'GURL'

static PROXY: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();

define_class!(
    // SAFETY:
    // - The superclass NSObject has no subclassing requirements.
    // - `UrlSchemeHandler` does not implement `Drop` and has no ivars.
    #[unsafe(super(NSObject))]
    #[name = "MuninnUrlSchemeHandler"]
    struct UrlSchemeHandler;

    impl UrlSchemeHandler {
        #[unsafe(method(handleGetURLEvent:withReplyEvent:))]
        fn handle_get_url(&self, event: *mut AnyObject, _reply: *mut AnyObject) {
            handle_get_url_event(event);
        }

        #[unsafe(method(applicationWillFinishLaunching:))]
        fn application_will_finish_launching(&self, _notification: *mut AnyObject) {
            register_get_url_handler(self);
        }
    }
);

fn handle_get_url_event(event: *mut AnyObject) {
    if event.is_null() {
        return;
    }
    let Some(url) = (unsafe { extract_url(event) }) else {
        warn!(target: TARGET_RUNTIME, "received muninn:// event without a URL string");
        return;
    };
    let Some(action) = parse_url_action(&url) else {
        warn!(target: TARGET_RUNTIME, %url, "ignored unrecognized muninn:// URL");
        return;
    };
    let Some(proxy) = PROXY.get() else {
        warn!(target: TARGET_RUNTIME, "muninn:// handler invoked before proxy was installed");
        return;
    };
    send_user_event(
        proxy,
        UserEvent::ExternalControl(action),
        "url_scheme_external_control",
    );
    info!(target: TARGET_RUNTIME, %url, "handled external-control muninn:// URL");
}

unsafe fn extract_url(event: *mut AnyObject) -> Option<String> {
    let descriptor: *mut AnyObject = msg_send![event, paramDescriptorForKeyword: KEY_DIRECT_OBJECT];
    if descriptor.is_null() {
        return None;
    }
    let string_value: *mut AnyObject = msg_send![descriptor, stringValue];
    if string_value.is_null() {
        return None;
    }
    let utf8: *const c_char = msg_send![string_value, UTF8String];
    if utf8.is_null() {
        return None;
    }
    CStr::from_ptr(utf8).to_str().ok().map(ToOwned::to_owned)
}

/// Install the `muninn://` `GetURL` Apple Event handler on the shared
/// `NSAppleEventManager`.
fn register_get_url_handler(handler: &UrlSchemeHandler) {
    let manager: *mut AnyObject =
        unsafe { msg_send![class!(NSAppleEventManager), sharedAppleEventManager] };
    if manager.is_null() {
        warn!(target: TARGET_RUNTIME, "NSAppleEventManager unavailable; muninn:// disabled");
        return;
    }

    unsafe {
        let _: () = msg_send![
            manager,
            setEventHandler: handler,
            andSelector: sel!(handleGetURLEvent:withReplyEvent:),
            forEventClass: INTERNET_EVENT_CLASS,
            andEventID: AE_GET_URL,
        ];
    }
    info!(target: TARGET_RUNTIME, "registered muninn:// URL scheme handler");
}

/// Register the `muninn://` URL scheme handler on the main thread.
///
/// Must be called once, from the main thread, *before* the event loop runs.
/// macOS dispatches the launch `GetURL` Apple Event during
/// `applicationWillFinishLaunching` — earlier than tao's `StartCause::Init` —
/// so we observe that notification to register the handler in time to catch the
/// URL that cold-launched the app. We also register immediately to cover the
/// already-running case.
pub(crate) fn install_url_scheme_handler(proxy: EventLoopProxy<UserEvent>) {
    if PROXY.set(proxy).is_err() {
        warn!(target: TARGET_RUNTIME, "muninn:// URL handler already installed");
        return;
    }

    let handler: Retained<UrlSchemeHandler> = unsafe { msg_send![UrlSchemeHandler::alloc(), init] };

    unsafe {
        let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        if center.is_null() {
            warn!(target: TARGET_RUNTIME, "NSNotificationCenter unavailable; muninn:// cold-launch may be missed");
        } else {
            let name: *mut AnyObject = msg_send![
                class!(NSString),
                stringWithUTF8String: b"NSApplicationWillFinishLaunchingNotification\0".as_ptr() as *const c_char
            ];
            let _: () = msg_send![
                center,
                addObserver: &*handler,
                selector: sel!(applicationWillFinishLaunching:),
                name: name,
                object: std::ptr::null::<AnyObject>(),
            ];
        }
    }

    register_get_url_handler(&handler);

    // The Apple Event Manager and notification center keep unretained
    // references to the handler, so it must outlive the process; intentionally
    // leak the single instance.
    std::mem::forget(handler);
}
