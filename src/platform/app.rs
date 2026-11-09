//! The application: AppKit's start-up, the delegate that hears about opened files, quitting
//! and the window's changes, the menu bar, and the event pump the render loop runs.

use std::cell::{Cell, OnceCell};
use std::path::PathBuf;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSApplicationPresentationOptions, NSApplicationTerminateReply, NSEventMask,
    NSEventModifierFlags, NSEventTrackingRunLoopMode, NSMenu, NSMenuItem, NSModalPanelRunLoopMode,
    NSWindow, NSWindowDelegate, NSWorkspace, NSWorkspaceWillPowerOffNotification,
};
use objc2_foundation::{
    NSArray, NSDate, NSDefaultRunLoopMode, NSNotification, NSObject, NSRunLoop, NSString, NSTimer,
    NSURL,
};

use super::{Event, dispatch, window};

pub use objc2::rc::autoreleasepool;

/// The menu bar's own commands; everything else lives in the design's menus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    About = 1,
    Settings,
    Quit,
    Open,
    OpenFolder,
    OpenNetwork,
    PlayPause,
    Minimize,
    MiniPlayer,
    NewTab,
    CloseTab,
    NextTab,
    PreviousTab,
}

impl Command {
    pub(super) const ALL: [Command; 13] = [
        Command::About,
        Command::Settings,
        Command::Quit,
        Command::Open,
        Command::OpenFolder,
        Command::OpenNetwork,
        Command::PlayPause,
        Command::Minimize,
        Command::MiniPlayer,
        Command::NewTab,
        Command::CloseTab,
        Command::NextTab,
        Command::PreviousTab,
    ];
}

thread_local! {
    static DELEGATE: OnceCell<Retained<Delegate>> = const { OnceCell::new() };
    /// AppKit has posted applicationDidFinishLaunching: files opened from Finder came first.
    static LAUNCHED: Cell<bool> = const { Cell::new(false) };
    /// The window closed for good: the pump says so, as WM_QUIT did.
    pub(super) static CLOSED: Cell<bool> = const { Cell::new(false) };
    /// The pass's events stop here, as `end_pump` asked.
    static END_PUMP: Cell<bool> = const { Cell::new(false) };
    /// The system is logging out or shutting down, which can't wait for wushang to save.
    static POWER_OFF: Cell<bool> = const { Cell::new(false) };
    /// Whether the menu bar's commands are on; off while the window's own dialogs have the keys.
    static MENU_ENABLED: Cell<bool> = const { Cell::new(true) };
    /// A pass runs from the timer inside AppKit's own loop, which mustn't be waited in.
    static IN_MODAL: Cell<bool> = const { Cell::new(false) };
    static MODAL_TIMER: OnceCell<Retained<NSTimer>> = const { OnceCell::new() };
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and Delegate has no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WushangDelegate"]
    pub(super) struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _: &NSNotification) {
            LAUNCHED.set(true);
        }

        /// Finder's Open and Open With, files dropped on the Dock icon, `open -a wushang`: they
        /// play, and the window comes back if it was minimized.
        #[unsafe(method(application:openURLs:))]
        fn open_urls(&self, _: &NSApplication, urls: &NSArray<NSURL>) {
            let paths: Vec<PathBuf> = urls
                .iter()
                .filter_map(|url| {
                    if url.isFileURL() {
                        url.path().map(|p| PathBuf::from(p.to_string()))
                    } else {
                        url.absoluteString().map(|s| PathBuf::from(s.to_string()))
                    }
                })
                .collect();
            if !paths.is_empty() {
                if LAUNCHED.get() {
                    window::deminiaturize();
                }
                dispatch(Event::Open(paths));
            }
        }

        /// Quit from the Dock or the menu bar's own Quit: the render loop ends and saves as
        /// it does for the window's close button, so AppKit isn't let to exit the process
        /// under it. Logging out doesn't wait, as it doesn't on Windows.
        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _: &NSApplication) -> NSApplicationTerminateReply {
            if POWER_OFF.get() || CLOSED.get() {
                return NSApplicationTerminateReply::TerminateNow;
            }
            dispatch(Event::Close);
            NSApplicationTerminateReply::TerminateCancel
        }

        /// A click on the Dock icon brings a minimized window back.
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn reopen(&self, _: &NSApplication, _visible: bool) -> bool {
            window::deminiaturize();
            true
        }

        #[unsafe(method(applicationDidChangeScreenParameters:))]
        fn screens_changed(&self, _: &NSNotification) {
            dispatch(Event::DisplayChanged);
        }
    }

    unsafe impl NSWindowDelegate for Delegate {
        #[unsafe(method(windowDidResize:))]
        fn did_resize(&self, _: &NSNotification) {
            window::note_frame();
            dispatch(Event::Resized);
        }

        #[unsafe(method(windowDidMove:))]
        fn did_move(&self, _: &NSNotification) {
            window::note_frame();
        }

        #[unsafe(method(windowDidChangeScreen:))]
        fn did_change_screen(&self, _: &NSNotification) {
            dispatch(Event::DisplayChanged);
        }

        #[unsafe(method(windowDidChangeBackingProperties:))]
        fn did_change_backing(&self, _: &NSNotification) {
            window::backing_changed();
            dispatch(Event::Resized);
            dispatch(Event::DisplayChanged);
        }

        #[unsafe(method(windowDidEndLiveResize:))]
        fn did_end_live_resize(&self, _: &NSNotification) {
            dispatch(Event::DisplayChanged);
        }

        #[unsafe(method(windowShouldClose:))]
        fn should_close(&self, _: &NSWindow) -> bool {
            dispatch(Event::Close);
            false
        }

        #[unsafe(method(windowWillEnterFullScreen:))]
        fn will_enter_full_screen(&self, _: &NSNotification) {
            window::set_fullscreen(window::Fullscreen::Entering);
        }

        #[unsafe(method(windowDidEnterFullScreen:))]
        fn did_enter_full_screen(&self, _: &NSNotification) {
            window::set_fullscreen(window::Fullscreen::On);
            dispatch(Event::Resized);
        }

        #[unsafe(method(windowWillExitFullScreen:))]
        fn will_exit_full_screen(&self, _: &NSNotification) {
            window::set_fullscreen(window::Fullscreen::Leaving);
        }

        #[unsafe(method(windowDidExitFullScreen:))]
        fn did_exit_full_screen(&self, _: &NSNotification) {
            window::set_fullscreen(window::Fullscreen::Off);
            dispatch(Event::Resized);
        }

        #[unsafe(method(windowDidFailToEnterFullScreen:))]
        fn did_fail_to_enter_full_screen(&self, _: &NSWindow) {
            window::set_fullscreen(window::Fullscreen::Off);
        }

        #[unsafe(method(windowDidFailToExitFullScreen:))]
        fn did_fail_to_exit_full_screen(&self, _: &NSWindow) {
            window::set_fullscreen(window::Fullscreen::On);
        }

        /// The menu bar and the Dock stay out of the way of the video, as Windows' borderless
        /// fullscreen covers the taskbar.
        #[unsafe(method(window:willUseFullScreenPresentationOptions:))]
        fn full_screen_options(
            &self,
            _: &NSWindow,
            _proposed: NSApplicationPresentationOptions,
        ) -> NSApplicationPresentationOptions {
            NSApplicationPresentationOptions::FullScreen
                | NSApplicationPresentationOptions::AutoHideMenuBar
                | NSApplicationPresentationOptions::AutoHideDock
                | NSApplicationPresentationOptions::AutoHideToolbar
        }
    }

    impl Delegate {
        #[unsafe(method(wushangMenu:))]
        fn menu(&self, sender: &NSMenuItem) {
            let tag = sender.tag();
            if let Some(command) = Command::ALL.into_iter().find(|c| *c as isize == tag) {
                dispatch(Event::Menu(command));
            }
        }

        /// Greys out the menu bar's commands while a dialog, Settings, the welcome, Customize
        /// or a menu of the window's own is open; Hide and Quit's neighbours stay AppKit's.
        /// Closing and switching tabs also wait for the window to be on screen: minimized, it
        /// isn't the key window, so their keys come here and would act on tabs out of sight.
        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> bool {
            let tabs = [Command::CloseTab, Command::NextTab, Command::PreviousTab];
            let on_tabs = tabs.into_iter().any(|c| c as isize == item.tag());
            item.action() != Some(sel!(wushangMenu:))
                || (MENU_ENABLED.get() && (!on_tabs || self.window_on_screen()))
        }

        #[unsafe(method(willPowerOff:))]
        fn will_power_off(&self, _: &NSNotification) {
            POWER_OFF.set(true);
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's init.
        unsafe { msg_send![super(this), init] }
    }

    /// Whether the player's window, the one this is the delegate of, is shown and not
    /// minimized. Before it exists, it isn't.
    fn window_on_screen(&self) -> bool {
        let ours = |w: &NSWindow| {
            w.delegate()
                .is_some_and(|d| std::ptr::addr_eq(Retained::as_ptr(&d), std::ptr::from_ref(self)))
        };
        NSApplication::sharedApplication(mtm())
            .windows()
            .iter()
            .any(|w| ours(&w) && w.isVisible() && !w.isMiniaturized())
    }
}

pub(super) fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("AppKit runs on the main thread")
}

pub(super) fn delegate() -> Retained<Delegate> {
    DELEGATE
        .with(|d| d.get().cloned())
        .expect("platform::init runs first")
}

pub(super) fn window_delegate() -> Retained<ProtocolObject<dyn NSWindowDelegate>> {
    ProtocolObject::from_retained(delegate())
}

/// Starts AppKit as a regular app with a Dock icon and menu bar. Returns the files Finder
/// asked to open, which arrive before AppKit finishes launching.
pub fn init() -> Vec<PathBuf> {
    let mtm = mtm();
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate = Delegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // SAFETY: the delegate lives as long as the app, and the selector takes a notification.
    unsafe {
        NSWorkspace::sharedWorkspace()
            .notificationCenter()
            .addObserver_selector_name_object(
                &delegate,
                sel!(willPowerOff:),
                Some(NSWorkspaceWillPowerOffNotification),
                None,
            );
    }
    DELEGATE.with(|d| _ = d.set(delegate.clone()));
    build_menu(mtm, &app, &delegate);
    app.finishLaunching();
    // Launched to open files, AppKit sends them in an Apple event just before
    // applicationDidFinishLaunching:. Started from a shell it may never come, so wait a little.
    let deadline = Instant::now() + Duration::from_millis(500);
    while !LAUNCHED.get() && Instant::now() < deadline {
        autoreleasepool(|_| {
            let until = NSDate::dateWithTimeIntervalSinceNow(0.05);
            // SAFETY: the default mode is a valid run loop mode.
            let event = unsafe {
                app.nextEventMatchingMask_untilDate_inMode_dequeue(
                    NSEventMask::Any,
                    Some(&until),
                    NSDefaultRunLoopMode,
                    true,
                )
            };
            if let Some(event) = event {
                app.sendEvent(&event);
            }
        });
    }
    #[cfg(debug_assertions)]
    super::remote::start();
    super::EARLY_OPEN.with_borrow_mut(std::mem::take)
}

/// Turns the menu bar's commands on or off; see `validateMenuItem:`.
pub fn set_menu_enabled(on: bool) {
    MENU_ENABLED.set(on);
}

/// Whether wushang was started hidden (`open -j`), which Windows' "Run: Minimized" stands for.
pub fn launched_hidden() -> bool {
    NSApplication::sharedApplication(mtm()).isHidden()
}

/// Runs this thread in the user-interactive class, the scheduler's highest for an app, so the
/// render loop wakes on time while decoders keep every core busy.
pub fn raise_thread_priority() {
    // SAFETY: changes only the calling thread's class.
    let done = unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0)
    };
    if done != 0 {
        eprintln!("wushang: cannot raise the render loop's priority ({done})");
    }
}

/// Leaves the events after this one for the next pass, so the render loop acts on what this
/// one asked for first: after a tab switch, keys go to the tab now on screen.
pub fn end_pump() {
    END_PUMP.set(true);
}

/// Whether `end_pump` was asked for in this pass, for the debug remote's lines.
#[cfg(debug_assertions)]
pub(super) fn pump_ended() -> bool {
    END_PUMP.get()
}

/// Handles pending events. Returns false once the window is gone.
pub fn pump_messages() -> bool {
    let app = NSApplication::sharedApplication(mtm());
    autoreleasepool(|_| {
        END_PUMP.set(false);
        let past = NSDate::distantPast();
        // SAFETY: the default mode is a valid run loop mode.
        while let Some(event) = unsafe {
            app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&past),
                NSDefaultRunLoopMode,
                true,
            )
        } {
            app.sendEvent(&event);
            if CLOSED.get() || END_PUMP.get() {
                break;
            }
        }
        #[cfg(debug_assertions)]
        if !CLOSED.get() && !END_PUMP.get() {
            super::remote::run();
        }
    });
    !CLOSED.get()
}

/// Waits up to `timeout` for an event, leaving it for the next pump. A pass from AppKit's
/// own loop returns at once.
pub fn wait_for_event(timeout: Duration) {
    if IN_MODAL.get() {
        return;
    }
    let app = NSApplication::sharedApplication(mtm());
    autoreleasepool(|_| {
        let until = NSDate::dateWithTimeIntervalSinceNow(timeout.as_secs_f64());
        // SAFETY: the default mode is a valid run loop mode.
        _ = unsafe {
            app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&until),
                NSDefaultRunLoopMode,
                false,
            )
        };
    });
}

/// Has `tick` run every few milliseconds while AppKit runs a loop of its own (a live resize,
/// a window drag, an open menu, a modal panel), where the render loop can't. Windows' version
/// was a timer set on WM_ENTERSIZEMOVE; these modes only exist while such a loop runs.
pub fn set_modal_tick(tick: fn()) {
    let block = RcBlock::new(move |_: NonNull<NSTimer>| {
        IN_MODAL.set(true);
        autoreleasepool(|_| {
            tick();
            // A test script can reach a file picker or menu, and close it.
            #[cfg(debug_assertions)]
            super::remote::run();
        });
        IN_MODAL.set(false);
    });
    // SAFETY: the block is 'static, and both modes are AppKit's own.
    unsafe {
        let timer = NSTimer::timerWithTimeInterval_repeats_block(1.0 / 240.0, true, &block);
        let run_loop = NSRunLoop::mainRunLoop();
        run_loop.addTimer_forMode(&timer, NSEventTrackingRunLoopMode);
        run_loop.addTimer_forMode(&timer, NSModalPanelRunLoopMode);
        MODAL_TIMER.with(|t| _ = t.set(timer));
    }
}

/// The last thing `main` does: the process may now end.
pub fn finish() {
    CLOSED.set(true);
}

/// The menu bar: the app menu, File, Playback and Window, each item doing what the design's
/// own menus do.
fn build_menu(mtm: MainThreadMarker, app: &NSApplication, delegate: &Delegate) {
    let command = |title: &str, command: Command, key: &str| {
        let item = item(mtm, title, Some(sel!(wushangMenu:)), key);
        // SAFETY: the delegate outlives the menu, and answers wushangMenu:.
        unsafe { item.setTarget(Some(delegate)) };
        item.setTag(command as isize);
        item
    };
    let bar = NSMenu::new(mtm);
    let submenu = |title: &str, items: Vec<Retained<NSMenuItem>>| {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
        for i in &items {
            menu.addItem(i);
        }
        let holder = NSMenuItem::new(mtm);
        holder.setSubmenu(Some(&menu));
        bar.addItem(&holder);
        menu
    };
    let separator = || NSMenuItem::separatorItem(mtm);
    let hide_others = item(mtm, "Hide Others", Some(sel!(hideOtherApplications:)), "h");
    hide_others
        .setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Option);
    submenu(
        "wushang",
        vec![
            command("About wushang", Command::About, ""),
            separator(),
            command("Settings\u{2026}", Command::Settings, ","),
            separator(),
            item(mtm, "Hide wushang", Some(sel!(hide:)), "h"),
            hide_others,
            item(mtm, "Show All", Some(sel!(unhideAllApplications:)), ""),
            separator(),
            command("Quit wushang", Command::Quit, "q"),
        ],
    );
    submenu(
        "File",
        vec![
            command("New Tab", Command::NewTab, "t"),
            command("Open File\u{2026}", Command::Open, "o"),
            command("Open Folder\u{2026}", Command::OpenFolder, "f"),
            command("Open Network Stream\u{2026}", Command::OpenNetwork, "n"),
            separator(),
            command("Close Tab", Command::CloseTab, "w"),
        ],
    );
    submenu(
        "Playback",
        vec![command("Play/Pause", Command::PlayPause, "")],
    );
    // Shift+Command+[ and ], as Safari and Finder have them.
    let tab_command = |title: &str, which: Command, key: &str| {
        let item = command(title, which, key);
        item.setKeyEquivalentModifierMask(
            NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
        );
        item
    };
    let window_menu = submenu(
        "Window",
        vec![
            command("Minimize", Command::Minimize, ""),
            command("Mini Player", Command::MiniPlayer, "m"),
            separator(),
            tab_command("Show Previous Tab", Command::PreviousTab, "["),
            tab_command("Show Next Tab", Command::NextTab, "]"),
            separator(),
        ],
    );
    app.setMainMenu(Some(&bar));
    // AppKit lists the window here, as other apps' Window menus do.
    app.setWindowsMenu(Some(&window_menu));
}

fn item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<Sel>,
    key: &str,
) -> Retained<NSMenuItem> {
    // SAFETY: the action, if any, is one AppKit or the delegate answers.
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(key),
        )
    }
}
