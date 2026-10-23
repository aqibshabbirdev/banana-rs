//! macOS: a 🍌 in the menu bar while Connect phone is on, so Banana Photos can keep serving
//! phones with its window minimised: "Open Banana Photos" brings the window back and "Quit"
//! quits. AppKit runs the menu itself, so it works while the app's UI loop is idle.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem};
use objc2_foundation::NSString;

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class doesn't implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "BananaMenuBarTarget"]
    struct Target;

    impl Target {
        #[unsafe(method(open:))]
        fn open(&self, _sender: Option<&AnyObject>) {
            show_window(self.mtm());
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
    }
);

impl Target {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

/// Brings the app forward with its window restored (it may be minimised to the Dock).
pub fn show_window(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    for w in app.windows().iter() {
        if w.isMiniaturized() {
            w.deminiaturize(None);
        }
        w.makeKeyAndOrderFront(None);
    }
}

/// The menu bar item; removed again when dropped.
pub struct MenuBar {
    item: Retained<NSStatusItem>,
    _target: Retained<Target>,
}

impl MenuBar {
    /// `status` is the first, greyed line of the menu, e.g. "Phones can connect".
    pub fn new(mtm: MainThreadMarker, status: &str) -> Self {
        let bar = NSStatusBar::systemStatusBar();
        // NSVariableStatusItemLength
        let item = bar.statusItemWithLength(-1.0);
        if let Some(button) = item.button(mtm) {
            button.setTitle(&NSString::from_str("🍌"));
            button.setToolTip(Some(&NSString::from_str("Banana Photos")));
        }
        let target = Target::new(mtm);
        let menu = NSMenu::new(mtm);
        let add = |title: &str, action: Option<objc2::runtime::Sel>, key: &str| {
            let it = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), action, &NSString::from_str(key))
            };
            if action.is_some() {
                unsafe { it.setTarget(Some(&target)) };
            } else {
                it.setEnabled(false);
            }
            menu.addItem(&it);
        };
        add(status, None, "");
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        add("Open Banana Photos", Some(sel!(open:)), "");
        add("Quit Banana Photos", Some(sel!(quit:)), "q");
        item.setMenu(Some(&menu));
        #[cfg(debug_assertions)]
        if std::env::var_os("BANANA_PHONES").is_some() {
            let frame = item.button(mtm).and_then(|b| b.window()).map(|w| w.frame());
            eprintln!("menu bar item: visible={} frame={frame:?}", item.isVisible());
        }
        Self { item, _target: target }
    }
}

impl Drop for MenuBar {
    fn drop(&mut self) {
        NSStatusBar::systemStatusBar().removeStatusItem(&self.item);
    }
}
