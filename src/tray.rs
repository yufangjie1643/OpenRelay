use crate::migration::migrate_legacy_data;
use crate::paths::AppPaths;
use crate::server::{port_from_env, serve_with_shutdown_and_static};
use std::ffi::OsStr;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::{Mutex, OnceLock};
use std::thread;
use tokio::sync::{mpsc, oneshot};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, TRUE, WPARAM,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, KillTimer, LoadIconW, LoadImageW, MessageBoxW,
    PostQuitMessage, RegisterClassW, SetForegroundWindow, SetTimer, TrackPopupMenu,
    TranslateMessage, CW_USEDEFAULT, HICON, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTSIZE,
    LR_LOADFROMFILE, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MF_SEPARATOR,
    MF_STRING, MSG, SW_SHOWNORMAL, TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, WM_APP, WM_COMMAND,
    WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONDBLCLK, WM_RBUTTONUP, WM_TIMER, WNDCLASSW,
};

pub const TRAY_TITLE: &str = "OpenRelay";
const MENU_OPEN_LABEL: &str = "打开管理面板";
const MENU_RESTART_LABEL: &str = "重启服务";
const MENU_EXIT_LABEL: &str = "退出 OpenRelay";

const WM_TRAYICON: u32 = WM_APP + 1;
const TIMER_STATUS: usize = 1;
const TRAY_UID: u32 = 1;
const ID_TRAY_OPEN: usize = 1001;
const ID_TRAY_RESTART: usize = 1002;
const ID_TRAY_EXIT: usize = 1003;

static TRAY_STATE: OnceLock<Mutex<TrayState>> = OnceLock::new();

struct TrayState {
    sender: mpsc::UnboundedSender<BackendCommand>,
    port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendCommand {
    Restart,
    Shutdown,
}

pub fn menu_labels() -> [&'static str; 3] {
    [MENU_OPEN_LABEL, MENU_RESTART_LABEL, MENU_EXIT_LABEL]
}

pub fn default_admin_url(port: u16) -> String {
    format!("http://localhost:{port}")
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let paths = AppPaths::from_env();
    let port = port_from_env();
    if let Err(err) = migrate_legacy_data(&paths.data_root, paths.legacy_root.as_deref()) {
        show_error_message(&format!("OpenRelay 配置初始化失败：\n{err}"));
        return Err(Box::new(err));
    }

    let mutex_name = to_wide("Local\\OpenRelayTrayManager");
    let mutex = unsafe { CreateMutexW(null(), TRUE, mutex_name.as_ptr()) };
    if mutex.is_null() {
        let err = io::Error::last_os_error();
        show_error_message(&format!("OpenRelay 单实例锁创建失败：\n{err}"));
        return Err(Box::new(err));
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        show_info_message("OpenRelay 已经在运行。");
        unsafe {
            CloseHandle(mutex);
        }
        return Ok(());
    }

    let (sender, receiver) = mpsc::unbounded_channel();
    let backend = spawn_backend(
        paths.data_root.clone(),
        paths.static_root.clone(),
        port,
        receiver,
    );
    TRAY_STATE
        .set(Mutex::new(TrayState {
            sender: sender.clone(),
            port,
        }))
        .map_err(|_| "tray state is already initialized")?;

    let result = unsafe { run_message_loop(&paths.static_root, port) };
    let _ = sender.send(BackendCommand::Shutdown);
    let _ = backend.join();
    unsafe {
        CloseHandle(mutex);
    }
    result
}

fn spawn_backend(
    root: PathBuf,
    static_root: PathBuf,
    port: u16,
    receiver: mpsc::UnboundedReceiver<BackendCommand>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(err) => {
                show_error_message(&format!("OpenRelay 后端运行时创建失败：\n{err}"));
                return;
            }
        };
        runtime.block_on(backend_loop(root, static_root, port, receiver));
    })
}

async fn backend_loop(
    root: PathBuf,
    static_root: PathBuf,
    port: u16,
    mut receiver: mpsc::UnboundedReceiver<BackendCommand>,
) {
    loop {
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let mut server = tokio::spawn(serve_with_shutdown_and_static(
            root.clone(),
            static_root.clone(),
            port,
            async move {
                let _ = shutdown_rx.await;
            },
        ));

        tokio::select! {
            result = &mut server => {
                report_server_result(result);
                match receiver.recv().await {
                    Some(BackendCommand::Restart) => continue,
                    Some(BackendCommand::Shutdown) | None => break,
                }
            }
            command = receiver.recv() => {
                match command {
                    Some(BackendCommand::Restart) => {
                        let _ = shutdown_tx.send(());
                        let _ = server.await;
                        continue;
                    }
                    Some(BackendCommand::Shutdown) | None => {
                        let _ = shutdown_tx.send(());
                        let _ = server.await;
                        break;
                    }
                }
            }
        }
    }
}

fn report_server_result(
    result: Result<Result<(), crate::server::ServerError>, tokio::task::JoinError>,
) {
    match result {
        Ok(Ok(())) => {}
        Ok(Err(err)) => show_error_message(&format!(
            "OpenRelay 服务启动失败：\n{err}\n\n可以右键托盘选择“重启服务”重试。"
        )),
        Err(err) => show_error_message(&format!(
            "OpenRelay 服务任务异常退出：\n{err}\n\n可以右键托盘选择“重启服务”重试。"
        )),
    }
}

unsafe fn run_message_loop(root: &Path, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let instance = GetModuleHandleW(null());
    if instance.is_null() {
        return Err(Box::new(io::Error::last_os_error()));
    }

    let class_name = to_wide("OpenRelayTrayWindow");
    let window_title = to_wide(TRAY_TITLE);
    let wc = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        lpszClassName: class_name.as_ptr(),
        ..std::mem::zeroed()
    };
    if RegisterClassW(&wc) == 0 {
        let err = io::Error::last_os_error();
        show_error_message(&format!("OpenRelay 托盘窗口注册失败：\n{err}"));
        return Err(Box::new(err));
    }

    let hwnd = CreateWindowExW(
        0,
        class_name.as_ptr(),
        window_title.as_ptr(),
        0,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        0,
        0,
        null_mut(),
        null_mut(),
        instance,
        null_mut(),
    );
    if hwnd.is_null() {
        let err = io::Error::last_os_error();
        show_error_message(&format!("OpenRelay 托盘窗口创建失败：\n{err}"));
        return Err(Box::new(err));
    }

    if !add_tray_icon(hwnd, root, port) {
        let err = io::Error::last_os_error();
        show_error_message(&format!("OpenRelay 托盘图标创建失败：\n{err}"));
        DestroyWindow(hwnd);
        return Err(Box::new(err));
    }

    SetTimer(hwnd, TIMER_STATUS, 5000, None);
    show_balloon(
        hwnd,
        TRAY_TITLE,
        &format!("管理面板: {}", default_admin_url(port)),
    );

    let mut msg: MSG = std::mem::zeroed();
    while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_COMMAND => {
            match wparam & 0xffff {
                ID_TRAY_OPEN => open_admin_panel(),
                ID_TRAY_RESTART => {
                    send_command(BackendCommand::Restart);
                    show_balloon(hwnd, TRAY_TITLE, "服务正在重启。");
                }
                ID_TRAY_EXIT => {
                    DestroyWindow(hwnd);
                }
                _ => {}
            }
            0
        }
        WM_TRAYICON => {
            let event = lparam as u32;
            if event == WM_RBUTTONUP || event == WM_CONTEXTMENU {
                show_tray_menu(hwnd);
            } else if event == WM_LBUTTONDBLCLK {
                open_admin_panel();
            }
            0
        }
        WM_TIMER => {
            if wparam == TIMER_STATUS {
                update_tray_tip(hwnd);
            }
            0
        }
        WM_DESTROY => {
            KillTimer(hwnd, TIMER_STATUS);
            remove_tray_icon(hwnd);
            send_command(BackendCommand::Shutdown);
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn add_tray_icon(hwnd: HWND, root: &Path, port: u16) -> bool {
    let mut nid = notify_data(hwnd);
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAYICON;
    nid.hIcon = load_icon(root);
    copy_wide_fixed(
        &mut nid.szTip,
        &format!("OpenRelay - {}", default_admin_url(port)),
    );
    Shell_NotifyIconW(NIM_ADD, &mut nid) != 0
}

unsafe fn remove_tray_icon(hwnd: HWND) {
    let mut nid = notify_data(hwnd);
    Shell_NotifyIconW(NIM_DELETE, &mut nid);
}

unsafe fn update_tray_tip(hwnd: HWND) {
    let port = current_port();
    let mut nid = notify_data(hwnd);
    nid.uFlags = NIF_TIP;
    copy_wide_fixed(
        &mut nid.szTip,
        &format!("OpenRelay - {}", default_admin_url(port)),
    );
    Shell_NotifyIconW(NIM_MODIFY, &mut nid);
}

unsafe fn show_balloon(hwnd: HWND, title: &str, message: &str) {
    let mut nid = notify_data(hwnd);
    nid.uFlags = NIF_INFO;
    nid.dwInfoFlags = NIIF_INFO;
    copy_wide_fixed(&mut nid.szInfoTitle, title);
    copy_wide_fixed(&mut nid.szInfo, message);
    Shell_NotifyIconW(NIM_MODIFY, &mut nid);
}

unsafe fn show_tray_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return;
    }

    let open = to_wide(MENU_OPEN_LABEL);
    let restart = to_wide(MENU_RESTART_LABEL);
    let exit = to_wide(MENU_EXIT_LABEL);
    AppendMenuW(menu, MF_STRING, ID_TRAY_OPEN, open.as_ptr());
    AppendMenuW(menu, MF_STRING, ID_TRAY_RESTART, restart.as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, ID_TRAY_EXIT, exit.as_ptr());

    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd);
    TrackPopupMenu(
        menu,
        TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
        pt.x,
        pt.y,
        0,
        hwnd,
        null(),
    );
    DestroyMenu(menu);
}

unsafe fn load_icon(root: &Path) -> HICON {
    let icon_path = root.join("assets").join("openrelay.ico");
    if icon_path.exists() {
        let wide_path = path_to_wide(&icon_path);
        let icon = LoadImageW(
            null_mut(),
            wide_path.as_ptr(),
            IMAGE_ICON,
            0,
            0,
            LR_LOADFROMFILE | LR_DEFAULTSIZE,
        );
        if !icon.is_null() {
            return icon;
        }
    }
    LoadIconW(null_mut(), IDI_APPLICATION)
}

unsafe fn notify_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_UID;
    nid
}

fn send_command(command: BackendCommand) {
    if let Some(state) = TRAY_STATE.get() {
        if let Ok(state) = state.lock() {
            let _ = state.sender.send(command);
        }
    }
}

fn current_port() -> u16 {
    TRAY_STATE
        .get()
        .and_then(|state| state.lock().ok().map(|state| state.port))
        .unwrap_or(18783)
}

unsafe fn open_admin_panel() {
    let url = to_wide(&default_admin_url(current_port()));
    let operation = to_wide("open");
    ShellExecuteW(
        null_mut(),
        operation.as_ptr(),
        url.as_ptr(),
        null(),
        null(),
        SW_SHOWNORMAL,
    );
}

fn show_info_message(message: &str) {
    unsafe {
        show_message(TRAY_TITLE, message, MB_OK | MB_ICONINFORMATION);
    }
}

fn show_error_message(message: &str) {
    unsafe {
        show_message(TRAY_TITLE, message, MB_OK | MB_ICONERROR);
    }
}

unsafe fn show_message(title: &str, message: &str, flags: u32) {
    let title = to_wide(title);
    let message = to_wide(message);
    MessageBoxW(
        null_mut(),
        message.as_ptr(),
        title.as_ptr(),
        flags | MB_SETFOREGROUND,
    );
}

fn copy_wide_fixed(dest: &mut [u16], value: &str) {
    dest.fill(0);
    let max = dest.len().saturating_sub(1);
    for (slot, ch) in dest.iter_mut().take(max).zip(value.encode_utf16()) {
        *slot = ch;
    }
}

fn to_wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn path_to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}
