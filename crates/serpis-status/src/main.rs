//! The menu bar icon that shows while this Mac's serpis node or terminals run. They outlive
//! serpis's window, so this is a small app of its own inside Serpis.app (with its own bundle id:
//! under Serpis's, macOS would answer opening Serpis by bringing this forward): the node starts it
//! (serpis_node::indicator), it asks `serpis-node status` every few seconds, and it leaves when
//! there is nothing left to show. Its Quit Serpis ends everything: the window, then the node and
//! terminals. `serpis-status --quit` does the same without asking.

mod view;

#[cfg(target_os = "macos")]
mod mac {
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::process::Command;

    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuItem, NSRunningApplication, NSStatusBar, NSStatusItem, NSVariableStatusItemLength};
    use objc2_foundation::{NSData, NSObject, NSSize, NSString, NSTimer, ns_string};

    use crate::view::Status;

    const ICON: &[u8] = include_bytes!("../../../assets/serpis-menubar.png");
    const BUNDLE_ID: &str = "dev.serpis.Serpis";
    /// How often it asks, in seconds.
    const EVERY: f64 = 4.0;
    /// Checks with nothing running before it leaves: a node restarting on a new build is back
    /// well within this.
    const PATIENCE: u32 = 4;
    /// Seconds the window gets to save its session and close.
    const WINDOW_CLOSES_IN: u64 = 15;

    struct Ivars {
        item: Retained<NSStatusItem>,
        headline: Retained<NSMenuItem>,
        summary: Retained<NSMenuItem>,
        idle_checks: Cell<u32>,
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[ivars = Ivars]
        struct Indicator;

        impl Indicator {
            #[unsafe(method(tick:))]
            fn tick(&self, _sender: Option<&AnyObject>) {
                self.refresh();
            }

            #[unsafe(method(open:))]
            fn open(&self, _sender: Option<&AnyObject>) {
                if let Some(app) = bundle() {
                    let _ = Command::new("open").arg(app).status();
                }
            }

            #[unsafe(method(quit:))]
            fn quit(&self, _sender: Option<&AnyObject>) {
                self.quit_serpis();
            }
        }
    );

    /// The app this helper is inside.
    fn bundle() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
        crate::view::app_of(&exe).map(PathBuf::from)
    }

    /// A `serpis-node` command, from the same app.
    fn node() -> Option<Command> {
        Some(Command::new(bundle()?.join("Contents/MacOS/serpis-node")))
    }

    /// None when the node beside this can't be asked (the app was moved or deleted).
    fn ask() -> Option<Status> {
        let out = node()?.arg("status").output().ok()?;
        Status::parse(String::from_utf8_lossy(&out.stdout).trim())
    }

    /// This app's windows that are open. Only this copy of the app: another copy, or a build run
    /// from source, isn't this one to close.
    fn open_windows() -> Vec<Retained<NSRunningApplication>> {
        let Some(window) = bundle().map(|b| b.join("Contents/MacOS/serpis")) else { return Vec::new() };
        let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(BUNDLE_ID));
        let is_window = |app: &NSRunningApplication| {
            let path = app.executableURL().and_then(|u| u.path()).map(|p| PathBuf::from(p.to_string()));
            path.and_then(|p| p.canonicalize().ok()).is_some_and(|p| p == window)
        };
        apps.iter().filter(|a| is_window(a)).collect()
    }

    fn alive(pid: i32) -> bool {
        Command::new("/bin/kill").args(["-0", &pid.to_string()]).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success())
    }

    /// Close the window as if it had been quit, so it saves its session, and wait for it: a window
    /// still open would start the node again. False if it is still there.
    fn close_windows() -> bool {
        let pids: Vec<i32> = open_windows()
            .iter()
            .map(|app| {
                app.terminate();
                app.processIdentifier()
            })
            .collect();
        for _ in 0..WINDOW_CLOSES_IN * 10 {
            if !pids.iter().any(|p| alive(*p)) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    }

    fn stop_node_and_terminals() {
        for what in ["stop", "stop-terminals"] {
            if let Some(mut node) = node() {
                let _ = node.arg(what).status();
            }
        }
    }

    /// `serpis-status --quit`: everything, without asking.
    pub fn quit_now() -> bool {
        if !close_windows() {
            eprintln!("Serpis's window didn't close, so the node keeps running");
            return false;
        }
        stop_node_and_terminals();
        true
    }

    impl Indicator {
        fn refresh(&self) {
            let iv = self.ivars();
            // No answer may only mean the app is being replaced by an update: as good as nothing
            // running, and it leaves if that lasts.
            let status = ask().unwrap_or_default();
            if ask_says_off(&status) {
                std::process::exit(0);
            }
            if !status.running() {
                let checks = iv.idle_checks.get() + 1;
                if checks >= PATIENCE {
                    std::process::exit(0);
                }
                iv.idle_checks.set(checks);
                return;
            }
            iv.idle_checks.set(0);
            let mtm = self.mtm();
            if let Some(button) = iv.item.button(mtm) {
                button.setTitle(&NSString::from_str(&status.title()));
            }
            iv.headline.setTitle(&NSString::from_str(status.headline()));
            iv.summary.setTitle(&NSString::from_str(&status.summary()));
        }

        fn quit_serpis(&self) {
            let mtm = self.mtm();
            let status = ask().unwrap_or_default();
            #[allow(deprecated)]
            NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
            if status.busy() {
                let alert = NSAlert::new(mtm);
                alert.setMessageText(ns_string!("Quit Serpis?"));
                alert.setInformativeText(&NSString::from_str(&status.quit_warning()));
                alert.addButtonWithTitle(ns_string!("Quit"));
                alert.addButtonWithTitle(ns_string!("Cancel"));
                if alert.runModal() != NSAlertFirstButtonReturn {
                    return;
                }
            }
            if !close_windows() {
                let alert = NSAlert::new(mtm);
                alert.setMessageText(ns_string!("Serpis's window didn't close"));
                alert.setInformativeText(ns_string!("The node keeps running. Close the window, then try again."));
                alert.runModal();
                return;
            }
            stop_node_and_terminals();
            std::process::exit(0);
        }
    }

    /// The setting is off. (A check that got no answer says nothing about it.)
    fn ask_says_off(status: &Status) -> bool {
        status.running() && !status.indicator
    }

    /// One indicator per user: held for as long as this process runs.
    fn only_copy() -> bool {
        let dir = std::env::var_os("SERPIS_NODE_DIR").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".serpis/node")));
        let Some(dir) = dir else { return true };
        if std::fs::create_dir_all(&dir).is_err() {
            return true;
        }
        let Ok(file) = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("status-indicator.lock")) else { return true };
        if file.try_lock().is_err() {
            return false;
        }
        std::mem::forget(file);
        true
    }

    fn item(title: &NSString, action: Option<objc2::runtime::Sel>, mtm: MainThreadMarker) -> Retained<NSMenuItem> {
        unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), title, action, ns_string!("")) }
    }

    pub fn run() {
        if !only_copy() {
            return;
        }
        let mtm = MainThreadMarker::new().expect("runs on the main thread");
        let app = NSApplication::sharedApplication(mtm);
        // No Dock icon and no menu of its own: only the status item.
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

        let status_item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = status_item.button(mtm) {
            let image = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(ICON));
            if let Some(image) = image {
                image.setSize(NSSize::new(16.0, 18.0));
                // macOS colors it to suit the menu bar.
                image.setTemplate(true);
                button.setImage(Some(&image));
            }
            button.setToolTip(Some(ns_string!("Serpis")));
        }

        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        let headline = item(ns_string!("Serpis"), None, mtm);
        headline.setEnabled(false);
        let summary = item(ns_string!(""), None, mtm);
        summary.setEnabled(false);
        let open = item(ns_string!("Open Serpis"), Some(sel!(open:)), mtm);
        let quit = item(ns_string!("Quit Serpis"), Some(sel!(quit:)), mtm);
        menu.addItem(&headline);
        menu.addItem(&summary);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&open);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&quit);
        status_item.setMenu(Some(&menu));

        let indicator: Retained<Indicator> = {
            let this = Indicator::alloc(mtm).set_ivars(Ivars { item: status_item, headline, summary, idle_checks: Cell::new(0) });
            unsafe { msg_send![super(this), init] }
        };
        unsafe {
            open.setTarget(Some(&indicator));
            quit.setTarget(Some(&indicator));
        }
        indicator.refresh();
        let _timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(EVERY, &indicator, sel!(tick:), None, true) };
        app.run();
    }
}

fn main() {
    #[cfg(target_os = "macos")]
    if std::env::args().nth(1).as_deref() == Some("--quit") {
        std::process::exit(if mac::quit_now() { 0 } else { 1 });
    } else {
        mac::run();
    }
    #[cfg(not(target_os = "macos"))]
    eprintln!("serpis-status is for the macOS menu bar");
}
