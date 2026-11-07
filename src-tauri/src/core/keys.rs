//! Windows/Linux keyboard shortcuts (Chrome's), as menu action ids for `on_menu`. On Windows the web view
//! has keyboard focus, so menu accelerators never fire; WebView2 hands us each key press instead.

/// `vk` = Windows virtual-key code. Returns the action id, or None to let the page have the key.
pub fn shortcut(vk: u32, ctrl: bool, shift: bool, alt: bool) -> Option<&'static str> {
    let letter = |c: u8| vk == c as u32;
    Some(match (ctrl, shift, alt) {
        (true, false, false) => match vk {
            _ if letter(b'T') => "new-tab",
            _ if letter(b'N') => "new-window",
            _ if letter(b'W') => "close-tab",
            0x73 => "close-tab", // F4
            _ if letter(b'L') => "focus-url",
            _ if letter(b'R') => "reload",
            _ if letter(b'F') => "find",
            _ if letter(b'P') => "print",
            _ if letter(b'D') => "bookmark",
            _ if letter(b'H') => "history",
            _ if letter(b'J') => "downloads",
            0x09 | 0x22 => "next-tab",                 // Tab, Page Down
            0x21 => "prev-tab",                        // Page Up
            0xBB | 0x6B => "zoom-in",                  // = (the + key), numpad +
            0xBD | 0x6D => "zoom-out",                 // -, numpad -
            0x30 | 0x60 => "zoom-reset",               // 0, numpad 0
            0x31..=0x38 => TAB_N[(vk - 0x31) as usize], // 1-8: that tab
            0x39 => "tab-last",                        // 9: last tab
            0xDB => "back",                            // [
            0xDD => "forward",                         // ]
            _ => return None,
        },
        (true, true, false) => match vk {
            _ if letter(b'N') => "new-incognito",
            _ if letter(b'W') => "close-window",
            _ if letter(b'B') => "bookmarks",
            0x09 => "prev-tab",
            0xBB => "zoom-in", // Ctrl+Shift+= is Ctrl++ on most layouts
            _ => return None,
        },
        (true, false, true) if letter(b'P') => "passwords",
        (false, false, true) => match vk {
            0x25 => "back",    // Left
            0x27 => "forward", // Right
            _ if letter(b'D') => "focus-url",
            _ => return None,
        },
        (false, false, false) => match vk {
            0x74 => "reload",    // F5
            0x75 => "focus-url", // F6
            _ => return None,
        },
        _ => return None,
    })
}

const TAB_N: [&str; 8] = ["tab-1", "tab-2", "tab-3", "tab-4", "tab-5", "tab-6", "tab-7", "tab-8"];

/// Held keys that repeat sensibly; everything else fires once per press (a held Ctrl+T must not open 30 tabs).
pub fn repeats(id: &str) -> bool { matches!(id, "next-tab" | "prev-tab" | "zoom-in" | "zoom-out") }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chrome_shortcuts() {
        assert_eq!(shortcut(0x09, true, false, false), Some("next-tab"));
        assert_eq!(shortcut(0x09, true, true, false), Some("prev-tab"));
        assert_eq!(shortcut(b'T' as u32, true, false, false), Some("new-tab"));
        assert_eq!(shortcut(b'N' as u32, true, true, false), Some("new-incognito"));
        assert_eq!(shortcut(0x31, true, false, false), Some("tab-1"));
        assert_eq!(shortcut(0x38, true, false, false), Some("tab-8"));
        assert_eq!(shortcut(0x39, true, false, false), Some("tab-last"));
        assert_eq!(shortcut(0x25, false, false, true), Some("back"));
        assert_eq!(shortcut(0x74, false, false, false), Some("reload"));
        // Typing and editing keys stay with the page.
        assert_eq!(shortcut(b'C' as u32, true, false, false), None);
        assert_eq!(shortcut(b'V' as u32, true, false, false), None);
        assert_eq!(shortcut(b'A' as u32, true, false, false), None);
        assert_eq!(shortcut(b'T' as u32, false, false, false), None);
        assert_eq!(shortcut(0x09, false, false, false), None);
        // AltGr arrives as Ctrl+Alt: AltGr+letters type characters on many layouts, only Ctrl+Alt+P is ours.
        assert_eq!(shortcut(b'Q' as u32, true, false, true), None);
        assert!(repeats("next-tab") && !repeats("new-tab"));
    }
}
