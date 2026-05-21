#[cfg(windows)]
mod windows_tray {
    use openrelay::tray::{default_admin_url, menu_labels, TRAY_TITLE};

    #[test]
    fn tray_menu_uses_chinese_openrelay_labels() {
        assert_eq!(TRAY_TITLE, "OpenRelay");
        assert_eq!(
            menu_labels(),
            ["打开管理面板", "重启服务", "退出 OpenRelay"]
        );
    }

    #[test]
    fn tray_admin_url_uses_existing_default_port() {
        assert_eq!(default_admin_url(18783), "http://localhost:18783");
    }
}
