use tauri::{
    AppHandle,
    menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu},
};

pub const QUIT_ID: &str = "qscope-quit";

pub fn menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    // 预定义退出项直接调用 NSApplication 的 terminate，普通菜单项允许先完成页面任务并释放后台连接。
    Menu::with_items(
        app,
        &[
            &Submenu::with_items(
                app,
                "QScope",
                true,
                &[
                    &PredefinedMenuItem::about(
                        app,
                        Some("About QScope"),
                        Some(AboutMetadata {
                            name: Some("QScope".into()),
                            version: Some(app.package_info().version.to_string()),
                            ..Default::default()
                        }),
                    )?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::services(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::hide(app, None)?,
                    &PredefinedMenuItem::hide_others(app, None)?,
                    &PredefinedMenuItem::show_all(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &MenuItem::with_id(app, QUIT_ID, "Quit QScope", true, Some("CmdOrCtrl+Q"))?,
                ],
            )?,
            &Submenu::with_items(
                app,
                "Edit",
                true,
                &[
                    &PredefinedMenuItem::undo(app, None)?,
                    &PredefinedMenuItem::redo(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::cut(app, None)?,
                    &PredefinedMenuItem::copy(app, None)?,
                    &PredefinedMenuItem::paste(app, None)?,
                    &PredefinedMenuItem::select_all(app, None)?,
                ],
            )?,
            &Submenu::with_items(
                app,
                "Window",
                true,
                &[
                    &PredefinedMenuItem::minimize(app, None)?,
                    &PredefinedMenuItem::maximize(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::close_window(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::bring_all_to_front(app, None)?,
                ],
            )?,
        ],
    )
}
