fn main() {
    // An app ACL manifest makes every command opt-in per webview (see capabilities/):
    // the toolbar and rb:// pages get the app commands, websites get only `page_event`.
    let commands = &[
        "ui_ready",
        "new_tab",
        "new_incognito_window",
        "close_tab",
        "activate_tab",
        "navigate",
        "go_back",
        "go_forward",
        "reload",
        "toggle_bookmark",
        "find_in_page",
        "suggest",
        "history_search",
        "history_clear",
        "bookmarks_list",
        "bookmark_remove",
        "downloads_list",
        "show_download",
        "tab_menu",
        "group_menu",
        "group_toggle",
        "group_rename",
        "move_tab",
        "tear_start",
        "tear_move",
        "tear_end",
        "attach_tab",
        "page_event",
        "password_save",
        "password_never",
        "password_dismiss",
        "password_menu",
        "passwords_list",
        "password_reveal",
        "password_delete",
        "passwords_import",
    ];
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(commands)))
        .expect("tauri build");
}
