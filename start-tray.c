/*
 * Build:
 *   windres start-tray.rc -O coff -o start-tray.res
 *   gcc start-tray.c start-tray.res -O2 -Wall -Wextra -mwindows -municode -o start-tray.exe
 */
#define WIN32_LEAN_AND_MEAN
#ifndef UNICODE
#define UNICODE
#endif
#ifndef _UNICODE
#define _UNICODE
#endif

#include <windows.h>
#include <shellapi.h>
#include <stdarg.h>
#include <stdio.h>
#include <wchar.h>

#define BUF_CCH 4096
#define WM_TRAYICON (WM_APP + 1)
#define TIMER_STATUS 1
#define ID_TRAY_OPEN 1001
#define ID_TRAY_RESTART 1002
#define ID_TRAY_EXIT 1003

typedef int (WINAPI *MessageBoxTimeoutWFn)(HWND, LPCWSTR, LPCWSTR, UINT, WORD, DWORD);

static wchar_t g_base_dir[BUF_CCH];
static wchar_t g_web_dir[BUF_CCH];
static wchar_t g_node_path[BUF_CCH];
static wchar_t g_server_path[BUF_CCH];
static PROCESS_INFORMATION g_service;
static HWND g_hwnd = NULL;
static NOTIFYICONDATAW g_nid;
static int g_service_running = 0;

static int write_text(wchar_t *out, size_t cch, const wchar_t *fmt, ...) {
  int n;
  va_list args;

  if (!out || cch == 0) return 0;

  va_start(args, fmt);
  n = _vsnwprintf(out, cch, fmt, args);
  va_end(args);

  out[cch - 1] = L'\0';
  return n >= 0 && (size_t)n < cch;
}

static int path_exists(const wchar_t *path) {
  DWORD attrs = GetFileAttributesW(path);
  return attrs != INVALID_FILE_ATTRIBUTES;
}

static int get_base_dir(wchar_t *out, size_t cch) {
  DWORD len;
  wchar_t *last_slash;

  if (!out || cch == 0) return 0;
  len = GetModuleFileNameW(NULL, out, (DWORD)cch);
  if (len == 0 || len >= cch) return 0;

  last_slash = wcsrchr(out, L'\\');
  if (!last_slash) last_slash = wcsrchr(out, L'/');
  if (!last_slash) return 0;
  *last_slash = L'\0';
  return 1;
}

static int join_path(wchar_t *out, size_t cch, const wchar_t *base, const wchar_t *tail) {
  size_t base_len = wcslen(base);
  const wchar_t *slash = L"\\";

  if (base_len > 0 && (base[base_len - 1] == L'\\' || base[base_len - 1] == L'/')) {
    slash = L"";
  }
  return write_text(out, cch, L"%ls%ls%ls", base, slash, tail);
}

static int find_node(wchar_t *out, size_t cch) {
  DWORD len = SearchPathW(NULL, L"node.exe", NULL, (DWORD)cch, out, NULL);
  return len > 0 && len < cch;
}

static void format_last_error(wchar_t *out, size_t cch, const wchar_t *label) {
  DWORD err = GetLastError();
  wchar_t msg[1024] = L"";

  FormatMessageW(
    FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
    NULL,
    err,
    0,
    msg,
    (DWORD)(sizeof(msg) / sizeof(msg[0])),
    NULL
  );

  write_text(out, cch, L"%ls failed (error %lu): %ls", label, err, msg[0] ? msg : L"unknown error");
}

static void show_message(const wchar_t *title, const wchar_t *message, UINT flags, DWORD timeout_ms) {
  HMODULE user32 = GetModuleHandleW(L"user32.dll");
  MessageBoxTimeoutWFn timeout_box = NULL;

  if (user32) {
    union {
      FARPROC proc;
      MessageBoxTimeoutWFn fn;
    } loader;
    loader.proc = GetProcAddress(user32, "MessageBoxTimeoutW");
    timeout_box = loader.fn;
  }

  if (timeout_box) {
    timeout_box(NULL, message, title, flags | MB_SETFOREGROUND, 0, timeout_ms);
  } else {
    MessageBoxW(NULL, message, title, flags | MB_SETFOREGROUND);
  }
}

static void show_balloon(const wchar_t *title, const wchar_t *message, DWORD icon) {
  if (!g_hwnd) return;
  g_nid.uFlags = NIF_INFO;
  wcsncpy(g_nid.szInfoTitle, title, sizeof(g_nid.szInfoTitle) / sizeof(g_nid.szInfoTitle[0]) - 1);
  wcsncpy(g_nid.szInfo, message, sizeof(g_nid.szInfo) / sizeof(g_nid.szInfo[0]) - 1);
  g_nid.dwInfoFlags = icon;
  Shell_NotifyIconW(NIM_MODIFY, &g_nid);
}

static int is_service_alive(void) {
  DWORD code;

  if (!g_service.hProcess) return 0;
  if (!GetExitCodeProcess(g_service.hProcess, &code)) return 0;
  return code == STILL_ACTIVE;
}

static void close_service_handles(void) {
  if (g_service.hThread) CloseHandle(g_service.hThread);
  if (g_service.hProcess) CloseHandle(g_service.hProcess);
  ZeroMemory(&g_service, sizeof(g_service));
  g_service_running = 0;
}

static int start_service(wchar_t *error, size_t error_cch) {
  STARTUPINFOW si;
  wchar_t cmd[BUF_CCH];

  if (is_service_alive()) return 1;
  close_service_handles();

  if (!find_node(g_node_path, BUF_CCH)) {
    write_text(error, error_cch, L"node.exe was not found in PATH.\nInstall Node.js or add it to PATH.");
    return 0;
  }

  if (!path_exists(g_server_path)) {
    write_text(error, error_cch, L"Web server entry not found:\n%ls", g_server_path);
    return 0;
  }

  ZeroMemory(&si, sizeof(si));
  ZeroMemory(&g_service, sizeof(g_service));
  si.cb = sizeof(si);
  si.dwFlags = STARTF_USESHOWWINDOW;
  si.wShowWindow = SW_HIDE;

  write_text(cmd, BUF_CCH, L"\"%ls\" \"%ls\"", g_node_path, g_server_path);

  if (!CreateProcessW(
      g_node_path,
      cmd,
      NULL,
      NULL,
      FALSE,
      CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP,
      NULL,
      g_web_dir,
      &si,
      &g_service)) {
    format_last_error(error, error_cch, L"CreateProcessW(node.exe)");
    ZeroMemory(&g_service, sizeof(g_service));
    return 0;
  }

  if (WaitForSingleObject(g_service.hProcess, 900) == WAIT_OBJECT_0) {
    DWORD code = 0;
    GetExitCodeProcess(g_service.hProcess, &code);
    write_text(error, error_cch, L"node server.js exited immediately (exit code %lu).\nPort 18783 may already be in use.", code);
    close_service_handles();
    return 0;
  }

  g_service_running = 1;
  return 1;
}

static void stop_service(void) {
  if (!g_service.hProcess) {
    close_service_handles();
    return;
  }

  if (is_service_alive()) {
    TerminateProcess(g_service.hProcess, 0);
    WaitForSingleObject(g_service.hProcess, 5000);
  }
  close_service_handles();
}

static void restart_service(void) {
  wchar_t error[BUF_CCH] = L"";

  stop_service();
  Sleep(350);
  if (start_service(error, BUF_CCH)) {
    show_balloon(L"LiteLLM Proxy", L"服务已重启并加载最新后端代码。", NIIF_INFO);
  } else {
    show_message(L"LiteLLM Proxy", error[0] ? error : L"重启失败。", MB_OK | MB_ICONERROR, 7000);
  }
}

static int check_environment(const wchar_t *base_dir) {
  wchar_t node_path[BUF_CCH];
  wchar_t server_path[BUF_CCH];

  join_path(server_path, BUF_CCH, base_dir, L"web\\server.js");

  if (!path_exists(server_path)) return 2;
  if (!find_node(node_path, BUF_CCH)) return 4;
  return 0;
}

static void open_web_ui(void) {
  ShellExecuteW(NULL, L"open", L"http://localhost:18783", NULL, NULL, SW_SHOWNORMAL);
}

static void update_tray_tip(void) {
  DWORD code = 0;

  if (g_service.hProcess && GetExitCodeProcess(g_service.hProcess, &code) && code == STILL_ACTIVE) {
    wcsncpy(g_nid.szTip, L"LiteLLM Proxy 运行中 - http://localhost:18783", sizeof(g_nid.szTip) / sizeof(g_nid.szTip[0]) - 1);
  } else {
    wcsncpy(g_nid.szTip, L"LiteLLM Proxy 已停止 - 右键可重启", sizeof(g_nid.szTip) / sizeof(g_nid.szTip[0]) - 1);
    g_service_running = 0;
  }
  g_nid.uFlags = NIF_TIP;
  Shell_NotifyIconW(NIM_MODIFY, &g_nid);
}

static void show_tray_menu(HWND hwnd) {
  POINT pt;
  HMENU menu = CreatePopupMenu();

  if (!menu) return;

  AppendMenuW(menu, MF_STRING, ID_TRAY_OPEN, L"打开 Web UI");
  AppendMenuW(menu, MF_STRING, ID_TRAY_RESTART, L"重启服务");
  AppendMenuW(menu, MF_SEPARATOR, 0, NULL);
  AppendMenuW(menu, MF_STRING, ID_TRAY_EXIT, L"退出");

  GetCursorPos(&pt);
  SetForegroundWindow(hwnd);
  TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, NULL);
  DestroyMenu(menu);
}

static int add_tray_icon(HWND hwnd) {
  HICON icon = NULL;
  wchar_t icon_path[BUF_CCH];

  join_path(icon_path, BUF_CCH, g_web_dir, L"icon.ico");
  if (path_exists(icon_path)) {
    icon = (HICON)LoadImageW(NULL, icon_path, IMAGE_ICON, 0, 0, LR_LOADFROMFILE | LR_DEFAULTSIZE);
  }
  if (!icon) icon = LoadIconW(NULL, IDI_APPLICATION);

  ZeroMemory(&g_nid, sizeof(g_nid));
  g_nid.cbSize = sizeof(g_nid);
  g_nid.hWnd = hwnd;
  g_nid.uID = 1;
  g_nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
  g_nid.uCallbackMessage = WM_TRAYICON;
  g_nid.hIcon = icon;
  wcsncpy(g_nid.szTip, L"LiteLLM Proxy 运行中 - http://localhost:18783", sizeof(g_nid.szTip) / sizeof(g_nid.szTip[0]) - 1);

  return Shell_NotifyIconW(NIM_ADD, &g_nid);
}

static void remove_tray_icon(void) {
  if (g_hwnd) Shell_NotifyIconW(NIM_DELETE, &g_nid);
}

static LRESULT CALLBACK window_proc(HWND hwnd, UINT msg, WPARAM wparam, LPARAM lparam) {
  switch (msg) {
    case WM_COMMAND:
      switch (LOWORD(wparam)) {
        case ID_TRAY_OPEN:
          open_web_ui();
          return 0;
        case ID_TRAY_RESTART:
          restart_service();
          return 0;
        case ID_TRAY_EXIT:
          DestroyWindow(hwnd);
          return 0;
      }
      break;
    case WM_TRAYICON:
      if (lparam == WM_RBUTTONUP || lparam == WM_CONTEXTMENU) {
        show_tray_menu(hwnd);
      } else if (lparam == WM_LBUTTONDBLCLK) {
        open_web_ui();
      }
      return 0;
    case WM_TIMER:
      if (wparam == TIMER_STATUS) update_tray_tip();
      return 0;
    case WM_DESTROY:
      KillTimer(hwnd, TIMER_STATUS);
      remove_tray_icon();
      stop_service();
      PostQuitMessage(0);
      return 0;
  }
  return DefWindowProcW(hwnd, msg, wparam, lparam);
}

int WINAPI wWinMain(HINSTANCE instance, HINSTANCE prev_instance, PWSTR cmd_line, int show_cmd) {
  HANDLE mutex;
  WNDCLASSW wc;
  MSG msg;
  wchar_t errors[BUF_CCH] = L"";

  (void)prev_instance;
  (void)cmd_line;
  (void)show_cmd;

  if (!get_base_dir(g_base_dir, BUF_CCH)) {
    show_message(L"LiteLLM Proxy", L"Unable to locate the manager directory.", MB_OK | MB_ICONERROR, 5000);
    return 1;
  }

  if (wcsstr(GetCommandLineW(), L"--check") != NULL) {
    return check_environment(g_base_dir);
  }

  mutex = CreateMutexW(NULL, TRUE, L"Local\\LiteLLMProxyTrayManager");
  if (!mutex || GetLastError() == ERROR_ALREADY_EXISTS) {
    show_message(L"LiteLLM Proxy", L"LiteLLM Proxy 管理器已经在运行。", MB_OK | MB_ICONINFORMATION, 4000);
    if (mutex) CloseHandle(mutex);
    return 0;
  }

  join_path(g_web_dir, BUF_CCH, g_base_dir, L"web");
  join_path(g_server_path, BUF_CCH, g_base_dir, L"web\\server.js");

  if (!start_service(errors, BUF_CCH)) {
    show_message(L"LiteLLM Proxy", errors[0] ? errors : L"服务启动失败。", MB_OK | MB_ICONERROR, 7000);
    CloseHandle(mutex);
    return 1;
  }

  ZeroMemory(&wc, sizeof(wc));
  wc.lpfnWndProc = window_proc;
  wc.hInstance = instance;
  wc.lpszClassName = L"LiteLLMProxyTrayManagerWindow";
  if (!RegisterClassW(&wc)) {
    format_last_error(errors, BUF_CCH, L"RegisterClassW");
    show_message(L"LiteLLM Proxy", errors, MB_OK | MB_ICONERROR, 7000);
    stop_service();
    CloseHandle(mutex);
    return 1;
  }

  g_hwnd = CreateWindowExW(0, wc.lpszClassName, L"LiteLLM Proxy", 0, 0, 0, 0, 0, NULL, NULL, instance, NULL);
  if (!g_hwnd) {
    format_last_error(errors, BUF_CCH, L"CreateWindowExW");
    show_message(L"LiteLLM Proxy", errors, MB_OK | MB_ICONERROR, 7000);
    stop_service();
    CloseHandle(mutex);
    return 1;
  }

  if (!add_tray_icon(g_hwnd)) {
    format_last_error(errors, BUF_CCH, L"Shell_NotifyIconW");
    show_message(L"LiteLLM Proxy", errors, MB_OK | MB_ICONERROR, 7000);
    DestroyWindow(g_hwnd);
    CloseHandle(mutex);
    return 1;
  }

  SetTimer(g_hwnd, TIMER_STATUS, 5000, NULL);
  show_balloon(L"LiteLLM Proxy", L"管理器已启动\nWeb UI / API: http://localhost:18783", NIIF_INFO);

  while (GetMessageW(&msg, NULL, 0, 0) > 0) {
    TranslateMessage(&msg);
    DispatchMessageW(&msg);
  }

  CloseHandle(mutex);
  return 0;
}
