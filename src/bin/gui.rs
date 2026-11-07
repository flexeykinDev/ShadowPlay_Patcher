//! Tray + mini-window GUI (native Win32, no UI framework).
//!
//! Runs quietly in the tray, shows whether Instant Replay is protected, can
//! start with Windows, and re-applies the patch whenever NVIDIA restarts.

#![windows_subsystem = "windows"]

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use shadowplay_patcher::memory::wide;
use shadowplay_patcher::patch::{self, PatchInfo, PatchStatus};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT,
    WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_DWORD, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW,
    RegSetKeyValueW,
};
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Controls::{
    DRAWITEMSTRUCT, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, ODS_DISABLED, ODS_FOCUS,
    ODS_NOFOCUSRECT, ODS_SELECTED,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_SYSTEM_AWARE, GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::Shell::{
    IsUserAnAdmin, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, ShellExecuteW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

// ----------------------------------------------------------------- constants
const APP_NAME: &str = "ShadowPlay Patcher";
const WND_CLASS: &str = "ShadowPlayPatcherWndClass";
const MUTEX_NAME: &str = "ShadowPlayPatcher_SingleInstance_v1";
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "ShadowPlayPatcher";
const SETTINGS_KEY: &str = "Software\\ShadowPlayPatcher";
const SETTING_AUTO_REAPPLY: &str = "AutoReapply";

const WM_TRAYICON: u32 = WM_APP + 1;
const TIMER_POLL: usize = 1;
const POLL_MS: u32 = 8000;

const ID_BTN_PATCH: u16 = 1001;
const ID_BTN_ADMIN: u16 = 1002;
const ID_TGL_AUTOSTART: u16 = 1003;
const ID_TGL_REAPPLY: u16 = 1004;
const IDM_OPEN: u16 = 2001;
const IDM_PATCH: u16 = 2002;
const IDM_AUTOSTART: u16 = 2003;
const IDM_REAPPLY: u16 = 2004;
const IDM_QUIT: u16 = 2005;

const TXT_PATCH: &str = "Patch now";
const TXT_ADMIN: &str = "Restart as administrator";
const TXT_AUTOSTART: &str = "Run when Windows starts";
const TXT_REAPPLY: &str = "Re-apply when NVIDIA restarts";
const TXT_NOTE: &str =
    "Windows that ask not to be captured, like password managers, can now show up in your clips.";

// Layout, in 96-dpi pixels.
const MARGIN: i32 = 16;
const CLIENT_W: i32 = 360;
const CARD_H: i32 = 84;
const BTN_Y: i32 = MARGIN + CARD_H + 12;
const BTN_H: i32 = 32;
const PATCH_W: i32 = 112;
const ADMIN_W: i32 = 196;
const TGL_Y: i32 = BTN_Y + BTN_H + 12;
const TGL_H: i32 = 28;
const NOTE_Y: i32 = TGL_Y + 2 * TGL_H + 10;
const NOTE_H: i32 = 34;
const CLIENT_H: i32 = NOTE_Y + NOTE_H + MARGIN;

const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

// Windows 11 light palette
const BG: u32 = rgb(243, 243, 243);
const CARD: u32 = rgb(251, 251, 251);
const EDGE: u32 = rgb(225, 225, 225);
const TEXT: u32 = rgb(26, 26, 26);
const SUB_TEXT: u32 = rgb(96, 100, 108);
const FAINT: u32 = rgb(150, 155, 165);
const DISABLED_FILL: u32 = rgb(232, 232, 232);
const SECONDARY_PRESSED: u32 = rgb(240, 240, 240);
const TGL_OFF_EDGE: u32 = rgb(140, 140, 140);
const TGL_OFF_KNOB: u32 = rgb(90, 90, 90);
const WHITE: u32 = rgb(255, 255, 255);

// ------------------------------------------------------------------- state
#[derive(Clone, Copy)]
struct Fonts {
    status: HFONT,
    body: HFONT,
    small: HFONT,
}

struct App {
    hwnd: Cell<HWND>,
    btn_patch: Cell<HWND>,
    btn_admin: Cell<HWND>,
    tgl_autostart: Cell<HWND>,
    tgl_reapply: Cell<HWND>,
    mutex: Cell<HANDLE>,
    fonts: Cell<Fonts>,
    bg_brush: Cell<HBRUSH>,
    tray_icon: Cell<HICON>,
    nid: Cell<NOTIFYICONDATAW>,
    dpi: Cell<i32>,
    is_admin: Cell<bool>,
    auto_reapply: Cell<bool>,
    autostart_on: Cell<bool>,
    shown_tray_hint: Cell<bool>,
    accent: Cell<u32>,
    accent_pressed: Cell<u32>,
    info: RefCell<PatchInfo>,
    last_check: RefCell<String>,
}

thread_local! {
    static APP: App = App {
        hwnd: Cell::new(null_mut()),
        btn_patch: Cell::new(null_mut()),
        btn_admin: Cell::new(null_mut()),
        tgl_autostart: Cell::new(null_mut()),
        tgl_reapply: Cell::new(null_mut()),
        mutex: Cell::new(null_mut()),
        fonts: Cell::new(Fonts { status: null_mut(), body: null_mut(), small: null_mut() }),
        bg_brush: Cell::new(null_mut()),
        tray_icon: Cell::new(null_mut()),
        nid: Cell::new(unsafe { zeroed() }),
        dpi: Cell::new(96),
        is_admin: Cell::new(false),
        auto_reapply: Cell::new(true),
        autostart_on: Cell::new(false),
        shown_tray_hint: Cell::new(false),
        accent: Cell::new(rgb(0, 95, 184)), // Win11 default accent (light)
        accent_pressed: Cell::new(rgb(0, 75, 150)),
        info: RefCell::new(PatchInfo { status: PatchStatus::NvidiaNotFound, message: "Checking...".into() }),
        last_check: RefCell::new(String::new()),
    };
}

fn app<R>(f: impl FnOnce(&App) -> R) -> R {
    APP.with(f)
}

/// Scale a 96-dpi metric to the current dpi (rounded, like MulDiv).
fn s(v: i32) -> i32 {
    mul_div(v, app(|a| a.dpi.get()), 96)
}

fn mul_div(a: i32, b: i32, c: i32) -> i32 {
    ((a as i64 * b as i64 + c as i64 / 2) / c as i64) as i32
}

/// Copy `text` into a fixed UTF-16 buffer, truncating and NUL-terminating.
fn copy_wide(dst: &mut [u16], text: &str) {
    let (mut n, max) = (0, dst.len() - 1);
    for (slot, c) in dst.iter_mut().zip(text.encode_utf16()).take(max) {
        *slot = c;
        n += 1;
    }
    dst[n] = 0;
}

fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
    RECT { left, top, right, bottom }
}

// ------------------------------------------------------------------ registry
// All values live under HKEY_CURRENT_USER.

fn reg_get_dword(key: &str, name: &str) -> Option<u32> {
    let (key, name) = (wide(key), wide(name));
    let (mut val, mut size) = (0u32, size_of::<u32>() as u32);
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_DWORD, null_mut(),
            (&raw mut val).cast(), &mut size)
    };
    (r == ERROR_SUCCESS).then_some(val)
}

fn reg_get_string(key: &str, name: &str) -> Option<String> {
    let (key, name) = (wide(key), wide(name));
    let mut buf = [0u16; 2048];
    let mut size = size_of_val(&buf) as u32;
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, null_mut(),
            buf.as_mut_ptr().cast(), &mut size)
    };
    if r != ERROR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn reg_set(key: &str, name: &str, kind: u32, data: *const c_void, size: usize) -> bool {
    let (key, name) = (wide(key), wide(name));
    unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), kind, data, size as u32) == ERROR_SUCCESS }
}

fn reg_set_dword(key: &str, name: &str, val: u32) -> bool {
    reg_set(key, name, REG_DWORD, (&raw const val).cast(), size_of::<u32>())
}

fn reg_set_string(key: &str, name: &str, val: &str) -> bool {
    let w = wide(val);
    reg_set(key, name, REG_SZ, w.as_ptr().cast(), w.len() * 2)
}

fn reg_delete(key: &str, name: &str) {
    let (key, name) = (wide(key), wide(name));
    unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
}

// --------------------------------------------------------------- settings
fn exe_path() -> String {
    let mut buf = [0u16; 1024];
    let n = unsafe { GetModuleFileNameW(null_mut(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

fn autostart_command() -> String {
    format!("\"{}\" --minimized", exe_path())
}

/// The Run entry as it is in the registry right now.
fn autostart_entry() -> Option<String> {
    reg_get_string(RUN_KEY, RUN_VALUE).filter(|v| !v.is_empty())
}

fn set_autostart(on: bool) {
    if on {
        reg_set_string(RUN_KEY, RUN_VALUE, &autostart_command());
    } else {
        reg_delete(RUN_KEY, RUN_VALUE);
    }
}

/// If autostart is on but points at an old location of the exe, fix it.
fn refresh_autostart_path() {
    if autostart_entry().is_some_and(|v| v != autostart_command()) {
        set_autostart(true);
    }
}

fn load_auto_reapply() -> bool {
    reg_get_dword(SETTINGS_KEY, SETTING_AUTO_REAPPLY).is_none_or(|v| v != 0)
}

fn load_accent_color() {
    // DWM stores the accent as 0xAABBGGRR, which is already COLORREF order.
    if let Some(v) = reg_get_dword("Software\\Microsoft\\Windows\\DWM", "AccentColor") {
        app(|a| a.accent.set(v & 0x00FF_FFFF));
    }
    let darker = |c: u32, amt: u8| {
        let ch = |shift: u32| (((c >> shift) & 0xFF) as u8).saturating_sub(amt);
        rgb(ch(0), ch(8), ch(16))
    };
    app(|a| a.accent_pressed.set(darker(a.accent.get(), 28)));
}

// --------------------------------------------------------------- status look
struct Palette {
    dot: u32,
    ring: u32,
    label: &'static str,
}

fn palette_for(status: PatchStatus) -> Palette {
    match status {
        PatchStatus::Patched => Palette { dot: rgb(46, 160, 67), ring: rgb(33, 120, 50), label: "Active" },
        PatchStatus::NotPatched => Palette { dot: rgb(232, 160, 40), ring: rgb(190, 130, 25), label: "Not applied" },
        PatchStatus::NeedsAdmin => Palette { dot: rgb(210, 70, 70), ring: rgb(175, 45, 45), label: "Access denied" },
        PatchStatus::Error => Palette { dot: rgb(210, 70, 70), ring: rgb(175, 45, 45), label: "Error" },
        PatchStatus::NvidiaNotFound => Palette { dot: rgb(150, 150, 150), ring: rgb(120, 120, 120), label: "NVIDIA not found" },
    }
}

/// The detail line under the status label.
fn status_message(info: &PatchInfo) -> String {
    if info.status == PatchStatus::NeedsAdmin && app(|a| a.is_admin.get()) {
        "Windows blocked access to the NVIDIA process, even with administrator rights.".into()
    } else {
        info.message.clone()
    }
}

fn wants_admin_button(status: PatchStatus) -> bool {
    status == PatchStatus::NeedsAdmin && !app(|a| a.is_admin.get())
}

// ------------------------------------------------------------------ drawing
unsafe fn ellipse(dc: HDC, r: &RECT, fill: u32, edge: u32, pen_w: i32) {
    unsafe {
        let b = CreateSolidBrush(fill);
        let p = CreatePen(PS_SOLID, pen_w, edge);
        let (ob, op) = (SelectObject(dc, b), SelectObject(dc, p));
        Ellipse(dc, r.left, r.top, r.right, r.bottom);
        SelectObject(dc, ob);
        SelectObject(dc, op);
        DeleteObject(b);
        DeleteObject(p);
    }
}

unsafe fn round_rect(dc: HDC, r: &RECT, radius: i32, fill: u32, edge: u32) {
    unsafe {
        let b = CreateSolidBrush(fill);
        let p = CreatePen(PS_SOLID, 1, edge);
        let (ob, op) = (SelectObject(dc, b), SelectObject(dc, p));
        RoundRect(dc, r.left, r.top, r.right, r.bottom, radius, radius);
        SelectObject(dc, ob);
        SelectObject(dc, op);
        DeleteObject(b);
        DeleteObject(p);
    }
}

unsafe fn draw_text(dc: HDC, text: &str, mut r: RECT, font: HFONT, color: u32, format: u32) {
    let w = wide(text);
    unsafe {
        SelectObject(dc, font);
        SetTextColor(dc, color);
        DrawTextW(dc, w.as_ptr(), -1, &mut r, format);
    }
}

/// A round status dot as a classic color+mask icon, so no .ico resource is needed.
fn make_dot_icon(p: &Palette) -> HICON {
    const N: i32 = 32;
    let dot = rect(3, 3, N - 3, N - 3);
    unsafe {
        let screen = GetDC(null_mut());
        let dc = CreateCompatibleDC(screen);
        let color = CreateCompatibleBitmap(screen, N, N);
        let mask = CreateBitmap(N, N, 1, 1, null());

        let old = SelectObject(dc, color);
        FillRect(dc, &rect(0, 0, N, N), GetStockObject(BLACK_BRUSH));
        ellipse(dc, &dot, p.dot, p.ring, 2);
        SelectObject(dc, mask);
        PatBlt(dc, 0, 0, N, N, WHITENESS);
        ellipse(dc, &dot, 0, 0, 1); // black = opaque in the mask
        SelectObject(dc, old);

        let ii = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&ii);
        DeleteObject(color);
        DeleteObject(mask);
        DeleteDC(dc);
        ReleaseDC(null_mut(), screen);
        icon
    }
}

unsafe fn paint_window(dc: HDC, client: &RECT) {
    let (info, last_check, fonts, bg) =
        app(|a| (a.info.borrow().clone(), a.last_check.borrow().clone(), a.fonts.get(), a.bg_brush.get()));
    let p = palette_for(info.status);
    let m = s(MARGIN);

    unsafe {
        FillRect(dc, client, bg);
        SetBkMode(dc, TRANSPARENT as _);
        let old_font = SelectObject(dc, fonts.body);

        // Status card: dot + label, last check on the right, detail below.
        let card = rect(m, m, client.right - m, m + s(CARD_H));
        round_rect(dc, &card, s(12), CARD, EDGE);

        let (dx, cy, dr) = (card.left + s(20), card.top + s(23), s(6));
        ellipse(dc, &rect(dx - dr, cy - dr, dx + dr, cy + dr), p.dot, p.ring, s(1));

        let top_row = |left, right| rect(left, card.top + s(10), right, card.top + s(36));
        draw_text(dc, p.label, top_row(dx + s(14), card.right - s(110)), fonts.status, TEXT,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER);
        if !last_check.is_empty() {
            draw_text(dc, &format!("Checked {last_check}"), top_row(card.right - s(130), card.right - s(14)),
                fonts.small, FAINT, DT_RIGHT | DT_SINGLELINE | DT_VCENTER);
        }

        draw_text(dc, &status_message(&info),
            rect(card.left + s(14), card.top + s(38), card.right - s(14), card.bottom - s(8)),
            fonts.body, SUB_TEXT, DT_LEFT | DT_WORDBREAK | DT_EDITCONTROL | DT_END_ELLIPSIS);

        draw_text(dc, TXT_NOTE, rect(m, s(NOTE_Y), client.right - m, s(NOTE_Y + NOTE_H)),
            fonts.small, SUB_TEXT, DT_LEFT | DT_WORDBREAK);

        SelectObject(dc, old_font);
    }
}

unsafe fn draw_focus(d: &DRAWITEMSTRUCT, r: &RECT) {
    if d.itemState & ODS_FOCUS != 0 && d.itemState & ODS_NOFOCUSRECT == 0 {
        unsafe { DrawFocusRect(d.hDC, r) };
    }
}

/// `primary`: filled with the accent color. Otherwise an outlined button.
unsafe fn draw_button(d: &DRAWITEMSTRUCT, text: &str, primary: bool) {
    let (bg, fonts, accent, accent_pressed) =
        app(|a| (a.bg_brush.get(), a.fonts.get(), a.accent.get(), a.accent_pressed.get()));
    let pressed = d.itemState & ODS_SELECTED != 0;
    let disabled = d.itemState & ODS_DISABLED != 0;

    let (fill, edge, color) = match (disabled, primary, pressed) {
        (true, _, _) => (DISABLED_FILL, DISABLED_FILL, FAINT),
        (false, true, false) => (accent, accent, WHITE),
        (false, true, true) => (accent_pressed, accent_pressed, WHITE),
        (false, false, false) => (CARD, EDGE, TEXT),
        (false, false, true) => (SECONDARY_PRESSED, EDGE, TEXT),
    };
    let r = d.rcItem;
    unsafe {
        FillRect(d.hDC, &r, bg);
        round_rect(d.hDC, &r, s(8), fill, edge);
        SetBkMode(d.hDC, TRANSPARENT as _);
        draw_text(d.hDC, text, r, fonts.body, color, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        let inset = s(3);
        draw_focus(d, &rect(r.left + inset, r.top + inset, r.right - inset, r.bottom - inset));
    }
}

unsafe fn draw_toggle(d: &DRAWITEMSTRUCT, on: bool, text: &str) {
    let (bg, fonts, accent) = app(|a| (a.bg_brush.get(), a.fonts.get(), a.accent.get()));
    let (r, dc) = (d.rcItem, d.hDC);
    unsafe {
        FillRect(dc, &r, bg);
        SetBkMode(dc, TRANSPARENT as _);

        let (track_w, track_h) = (s(36), s(18));
        let ty = r.top + (r.bottom - r.top - track_h) / 2;
        let tx = r.left;
        let (track, edge, knob) = if on { (accent, accent, WHITE) } else { (WHITE, TGL_OFF_EDGE, TGL_OFF_KNOB) };
        round_rect(dc, &rect(tx, ty, tx + track_w, ty + track_h), track_h, track, edge);

        let pad = s(4);
        let kd = track_h - 2 * pad;
        let kx = if on { tx + track_w - pad - kd } else { tx + pad };
        ellipse(dc, &rect(kx, ty + pad, kx + kd, ty + pad + kd), knob, knob, 1);

        let mut text_r = rect(tx + track_w + s(10), r.top, r.right, r.bottom);
        draw_text(dc, text, text_r, fonts.body, TEXT, DT_LEFT | DT_SINGLELINE | DT_VCENTER);

        // Focus rect hugs the label, not the whole row.
        let w = wide(text);
        let mut size = zeroed();
        GetTextExtentPoint32W(dc, w.as_ptr(), w.len() as i32 - 1, &mut size);
        text_r.left -= s(3);
        text_r.right = text_r.left + size.cx + s(6);
        text_r.top += s(3);
        text_r.bottom -= s(3);
        draw_focus(d, &text_r);
    }
}

// ------------------------------------------------------------------ tray
fn tray_add() {
    let mut nid: NOTIFYICONDATAW = unsafe { zeroed() };
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = app(|a| a.hwnd.get());
    nid.uID = 1;
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAYICON;
    nid.hIcon = app(|a| a.tray_icon.get());
    copy_wide(&mut nid.szTip, APP_NAME);
    app(|a| a.nid.set(nid));
    unsafe { Shell_NotifyIconW(NIM_ADD, &nid) };
}

fn tray_modify(edit: impl FnOnce(&mut NOTIFYICONDATAW)) {
    let mut nid = app(|a| a.nid.get());
    edit(&mut nid);
    app(|a| a.nid.set(nid));
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
}

fn tray_balloon(text: &str) {
    tray_modify(|nid| {
        nid.uFlags = NIF_INFO;
        copy_wide(&mut nid.szInfoTitle, APP_NAME);
        copy_wide(&mut nid.szInfo, text);
        nid.dwInfoFlags = NIIF_INFO;
    });
}

fn tray_remove() {
    let nid = app(|a| a.nid.get());
    unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
}

// --------------------------------------------------------------- state
fn now_string() -> String {
    let st = unsafe {
        let mut st = zeroed();
        GetLocalTime(&mut st);
        st
    };
    format!("{:02}:{:02}:{:02}", st.wHour, st.wMinute, st.wSecond)
}

/// Store a new status and bring tray icon, buttons and window in line with it.
fn set_info(info: PatchInfo) {
    let status = info.status;
    app(|a| {
        *a.info.borrow_mut() = info;
        *a.last_check.borrow_mut() = now_string();
    });

    let p = palette_for(status);
    let icon = make_dot_icon(&p);
    let old = app(|a| a.tray_icon.replace(icon));
    tray_modify(|nid| {
        nid.uFlags = NIF_ICON | NIF_TIP;
        nid.hIcon = icon;
        copy_wide(&mut nid.szTip, &format!("{APP_NAME} - {}", p.label));
    });

    let (hwnd, btn_patch, btn_admin) = app(|a| (a.hwnd.get(), a.btn_patch.get(), a.btn_admin.get()));
    unsafe {
        SendMessageW(hwnd, WM_SETICON, ICON_SMALL as WPARAM, icon as LPARAM);
        SendMessageW(hwnd, WM_SETICON, ICON_BIG as WPARAM, icon as LPARAM);
        if !old.is_null() {
            DestroyIcon(old);
        }
        // Nothing to patch while Active.
        EnableWindow(btn_patch, (status != PatchStatus::Patched) as i32);
        ShowWindow(btn_admin, if wants_admin_button(status) { SW_SHOW } else { SW_HIDE });
        InvalidateRect(hwnd, null(), 0);
        InvalidateRect(btn_patch, null(), 0);
    }
}

fn refresh_state() {
    let mut info = patch::query_status();
    if app(|a| a.auto_reapply.get()) && info.status == PatchStatus::NotPatched {
        info = patch::apply_patch();
    }
    set_info(info);
}

fn window_visible() -> bool {
    unsafe { IsWindowVisible(app(|a| a.hwnd.get())) != 0 }
}

fn do_patch_now() {
    set_info(patch::apply_patch());
    // The window already shows the result; from the tray, say it in a balloon.
    if !window_visible() {
        let msg = app(|a| status_message(&a.info.borrow()));
        tray_balloon(&msg);
    }
}

fn toggle_autostart() {
    let wanted = !app(|a| a.autostart_on.get());
    set_autostart(wanted);
    // Show what the registry really holds, not what we hoped to write.
    let actual = autostart_entry().is_some();
    app(|a| a.autostart_on.set(actual));
    if actual != wanted {
        tray_balloon("Could not change the Windows startup setting.");
    }
    unsafe { InvalidateRect(app(|a| a.tgl_autostart.get()), null(), 0) };
}

fn toggle_auto_reapply() {
    let on = !app(|a| a.auto_reapply.get());
    app(|a| a.auto_reapply.set(on));
    reg_set_dword(SETTINGS_KEY, SETTING_AUTO_REAPPLY, on as u32);
    unsafe { InvalidateRect(app(|a| a.tgl_reapply.get()), null(), 0) };
    if on {
        refresh_state();
    }
}

fn create_single_instance_mutex() -> HANDLE {
    let name = wide(MUTEX_NAME);
    unsafe { CreateMutexW(null(), 0, name.as_ptr()) }
}

fn restart_as_admin() {
    // Let go of the single-instance mutex first, or the elevated copy would
    // think we are still running and exit at once.
    let mutex = app(|a| a.mutex.replace(null_mut()));
    if !mutex.is_null() {
        unsafe { CloseHandle(mutex) };
    }

    let hwnd = app(|a| a.hwnd.get());
    let (verb, exe) = (wide("runas"), wide(&exe_path()));
    let r = unsafe { ShellExecuteW(hwnd, verb.as_ptr(), exe.as_ptr(), null(), null(), SW_SHOWNORMAL) };
    if r as isize > 32 {
        unsafe { DestroyWindow(hwnd) };
    } else {
        // UAC prompt cancelled or failed: keep running as we were.
        app(|a| a.mutex.set(create_single_instance_mutex()));
    }
}

// ------------------------------------------------------------------ window
fn show_window() {
    let hwnd = app(|a| a.hwnd.get());
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }
}

fn show_context_menu() {
    let (hwnd, autostart, reapply, status) =
        app(|a| (a.hwnd.get(), a.autostart_on.get(), a.auto_reapply.get(), a.info.borrow().status));
    let checked = |on: bool| if on { MF_CHECKED } else { 0 };
    let patch_flags = if status == PatchStatus::Patched { MF_GRAYED } else { 0 };
    let items: [(u32, u16, &str); 7] = [
        (MF_STRING, IDM_OPEN, "Open"),
        (MF_STRING | patch_flags, IDM_PATCH, TXT_PATCH),
        (MF_SEPARATOR, 0, ""),
        (MF_STRING | checked(autostart), IDM_AUTOSTART, TXT_AUTOSTART),
        (MF_STRING | checked(reapply), IDM_REAPPLY, TXT_REAPPLY),
        (MF_SEPARATOR, 0, ""),
        (MF_STRING, IDM_QUIT, "Quit"),
    ];
    unsafe {
        let menu = CreatePopupMenu();
        for (flags, id, text) in items {
            let w = wide(text);
            let text_ptr = if flags & MF_SEPARATOR != 0 { null() } else { w.as_ptr() };
            AppendMenuW(menu, flags, id as usize, text_ptr);
        }
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        SetForegroundWindow(hwnd);
        TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, null());
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
    }
}

fn make_font(face: &str, pt: i32, weight: u32) -> HFONT {
    let dpi = app(|a| a.dpi.get());
    let w = wide(face);
    unsafe {
        CreateFontW(-mul_div(pt, dpi, 72), 0, 0, 0, weight as i32, 0, 0, 0, DEFAULT_CHARSET as _,
            OUT_DEFAULT_PRECIS as _, CLIP_DEFAULT_PRECIS as _, CLEARTYPE_QUALITY as _,
            (DEFAULT_PITCH | FF_DONTCARE) as _, w.as_ptr())
    }
}

fn create_button(parent: HWND, text: &str, id: u16, x: i32, y: i32, w: i32, h: i32) -> HWND {
    let class = wide("BUTTON");
    let text = wide(text);
    unsafe {
        CreateWindowExW(0, class.as_ptr(), text.as_ptr(), WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW as u32,
            s(x), s(y), s(w), s(h), parent, id as usize as HMENU, GetModuleHandleW(null()), null())
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            app(|a| {
                a.hwnd.set(hwnd);
                a.dpi.set(unsafe { GetDpiForWindow(hwnd) } as i32);
            });
            let fonts = Fonts {
                status: make_font("Segoe UI Variable Text", 12, FW_SEMIBOLD),
                body: make_font("Segoe UI Variable Text", 10, FW_NORMAL),
                small: make_font("Segoe UI Variable Small", 9, FW_NORMAL),
            };
            app(|a| a.fonts.set(fonts));

            let row_w = CLIENT_W - 2 * MARGIN;
            let patch = create_button(hwnd, TXT_PATCH, ID_BTN_PATCH, MARGIN, BTN_Y, PATCH_W, BTN_H);
            let admin = create_button(hwnd, TXT_ADMIN, ID_BTN_ADMIN, MARGIN + PATCH_W + 8, BTN_Y, ADMIN_W, BTN_H);
            unsafe { ShowWindow(admin, SW_HIDE) };
            let t1 = create_button(hwnd, TXT_AUTOSTART, ID_TGL_AUTOSTART, MARGIN, TGL_Y, row_w, TGL_H);
            let t2 = create_button(hwnd, TXT_REAPPLY, ID_TGL_REAPPLY, MARGIN, TGL_Y + TGL_H, row_w, TGL_H);
            app(|a| {
                a.btn_patch.set(patch);
                a.btn_admin.set(admin);
                a.tgl_autostart.set(t1);
                a.tgl_reapply.set(t2);
            });
            0
        }

        WM_DRAWITEM => {
            let d = unsafe { &*(lp as *const DRAWITEMSTRUCT) };
            let (autostart, reapply) = app(|a| (a.autostart_on.get(), a.auto_reapply.get()));
            unsafe {
                match d.CtlID as u16 {
                    ID_BTN_PATCH => draw_button(d, TXT_PATCH, true),
                    ID_BTN_ADMIN => draw_button(d, TXT_ADMIN, false),
                    ID_TGL_AUTOSTART => draw_toggle(d, autostart, TXT_AUTOSTART),
                    ID_TGL_REAPPLY => draw_toggle(d, reapply, TXT_REAPPLY),
                    _ => {}
                }
            }
            1
        }

        WM_COMMAND => {
            match (wp & 0xFFFF) as u16 {
                ID_BTN_PATCH | IDM_PATCH => do_patch_now(),
                ID_BTN_ADMIN => restart_as_admin(),
                IDM_OPEN => show_window(),
                ID_TGL_AUTOSTART | IDM_AUTOSTART => toggle_autostart(),
                ID_TGL_REAPPLY | IDM_REAPPLY => toggle_auto_reapply(),
                IDM_QUIT => unsafe {
                    DestroyWindow(hwnd);
                },
                _ => {}
            }
            0
        }

        WM_TRAYICON => {
            match (lp as u32) & 0xFFFF {
                WM_LBUTTONDBLCLK => show_window(),
                WM_RBUTTONUP | WM_CONTEXTMENU => show_context_menu(),
                _ => {}
            }
            0
        }

        WM_TIMER => {
            if wp == TIMER_POLL {
                refresh_state();
            }
            0
        }

        WM_PAINT => unsafe {
            // Double-buffered to avoid flicker.
            let mut ps: PAINTSTRUCT = zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut client = rect(0, 0, 0, 0);
            GetClientRect(hwnd, &mut client);
            let mem = CreateCompatibleDC(hdc);
            let bmp = CreateCompatibleBitmap(hdc, client.right, client.bottom);
            let old = SelectObject(mem, bmp);
            paint_window(mem, &client);
            BitBlt(hdc, 0, 0, client.right, client.bottom, mem, 0, 0, SRCCOPY);
            SelectObject(mem, old);
            DeleteObject(bmp);
            DeleteDC(mem);
            EndPaint(hwnd, &ps);
            0
        },

        WM_CLOSE => {
            unsafe { ShowWindow(hwnd, SW_HIDE) };
            if !app(|a| a.shown_tray_hint.replace(true)) {
                tray_balloon("Still running here. Right-click the icon for options, or double-click to reopen.");
            }
            0
        }

        WM_DESTROY => {
            unsafe { KillTimer(hwnd, TIMER_POLL) };
            tray_remove();
            unsafe { PostQuitMessage(0) };
            0
        }

        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn main() {
    let has_arg = |name: &str| std::env::args().skip(1).any(|a| a == name);
    let class = wide(WND_CLASS);

    let mutex = create_single_instance_mutex();
    if !mutex.is_null() && unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let existing = FindWindowW(class.as_ptr(), null());
            if !existing.is_null() {
                ShowWindow(existing, SW_SHOW);
                SetForegroundWindow(existing);
            }
        }
        return;
    }

    refresh_autostart_path();
    app(|a| {
        a.mutex.set(mutex);
        a.is_admin.set(unsafe { IsUserAnAdmin() } != 0);
        a.autostart_on.set(autostart_entry().is_some());
        // --no-apply: observe only for this run (not saved). "Patch now" still works.
        a.auto_reapply.set(load_auto_reapply() && !has_arg("--no-apply"));
    });

    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_SYSTEM_AWARE);
        let hinst = GetModuleHandleW(null());
        load_accent_color();
        let icc = INITCOMMONCONTROLSEX { dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_STANDARD_CLASSES };
        InitCommonControlsEx(&icc);

        let bg = CreateSolidBrush(BG);
        let icon = make_dot_icon(&palette_for(PatchStatus::NvidiaNotFound));
        app(|a| {
            a.bg_brush.set(bg);
            a.tray_icon.set(icon);
        });

        let mut wc: WNDCLASSEXW = zeroed();
        wc.cbSize = size_of::<WNDCLASSEXW>() as u32;
        wc.lpfnWndProc = Some(wnd_proc);
        wc.hInstance = hinst;
        wc.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
        wc.hbrBackground = bg;
        wc.lpszClassName = class.as_ptr();
        wc.hIcon = icon;
        RegisterClassExW(&wc);

        app(|a| a.dpi.set(GetDpiForSystem() as i32));
        let mut r = rect(0, 0, s(CLIENT_W), s(CLIENT_H));
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        AdjustWindowRectEx(&mut r, style, 0, 0);

        let title = wide(APP_NAME);
        let hwnd = CreateWindowExW(0, class.as_ptr(), title.as_ptr(), style, CW_USEDEFAULT, CW_USEDEFAULT,
            r.right - r.left, r.bottom - r.top, null_mut(), null_mut(), hinst, null());

        tray_add();
        refresh_state();
        SetTimer(hwnd, TIMER_POLL, POLL_MS, None);

        ShowWindow(hwnd, if has_arg("--minimized") { SW_HIDE } else { SW_SHOW });
        UpdateWindow(hwnd);

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(hwnd, &msg) != 0 {
                continue;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let icon = app(|a| a.tray_icon.get());
        if !icon.is_null() {
            DestroyIcon(icon);
        }
        DeleteObject(bg);
        let mutex = app(|a| a.mutex.get());
        if !mutex.is_null() {
            CloseHandle(mutex);
        }
    }
}
