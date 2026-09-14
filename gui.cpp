// ShadowPlay Patcher - tray + mini-window GUI (native Win32, no dependencies).
//
// Fixes the real annoyance of the CLI: it runs quietly in the tray, shows at a
// glance whether Instant Replay is protected, can start with Windows, and
// re-applies the patch automatically whenever the NVIDIA process restarts.
//
// Styling follows Windows 11 / Fluent: system accent color, a filled primary
// button with rounded corners, owner-drawn toggle switches, a subtle status
// card, Segoe UI Variable typography, and consistent 20px gutters.

#include "patch.h"

#include <windows.h>
#include <shellapi.h>
#include <commctrl.h>
#include <string>

#pragma comment(lib, "comctl32.lib")
// Opt into the v6 common controls so any native controls get the modern look.
#pragma comment(linker, "\"/manifestdependency:type='win32' "                 \
    "name='Microsoft.Windows.Common-Controls' version='6.0.0.0' "             \
    "processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'\"")

// ----------------------------------------------------------------- constants
static const wchar_t* kAppName   = L"ShadowPlay Patcher";
static const wchar_t* kWndClass  = L"ShadowPlayPatcherWndClass";
static const wchar_t* kMutexName = L"ShadowPlayPatcher_SingleInstance_v1";
static const wchar_t* kRunValue  = L"ShadowPlayPatcher";
static const wchar_t* kRunKey    = L"Software\\Microsoft\\Windows\\CurrentVersion\\Run";

#define WM_TRAYICON   (WM_APP + 1)
#define TIMER_POLL    1
#define POLL_MS       8000

enum {
    ID_BTN_PATCH = 1001,
    ID_TGL_AUTOSTART,
    ID_TGL_REAPPLY,
    IDM_OPEN = 2001,
    IDM_PATCH,
    IDM_AUTOSTART,
    IDM_REAPPLY,
    IDM_QUIT
};

// Fluent light palette
static const COLORREF kBg        = RGB(243, 243, 243); // window background
static const COLORREF kCard      = RGB(251, 251, 251); // status card fill
static const COLORREF kCardEdge  = RGB(225, 225, 225); // hairline border
static const COLORREF kText      = RGB(26, 26, 26);
static const COLORREF kSubText   = RGB(96, 100, 108);
static const COLORREF kFaint     = RGB(150, 155, 165);
static const COLORREF kTglOffTrk = RGB(255, 255, 255);
static const COLORREF kTglOffEdge= RGB(140, 140, 140);
static const COLORREF kTglOffKnob= RGB(90, 90, 90);

// ------------------------------------------------------------------- globals
static HINSTANCE g_hInst   = nullptr;
static HWND      g_hwnd    = nullptr;
static HWND      g_btnPatch = nullptr, g_tglAutostart = nullptr, g_tglReapply = nullptr;
static HFONT     g_headerFont = nullptr, g_subFont = nullptr, g_statusFont = nullptr,
                 g_bodyFont = nullptr, g_smallFont = nullptr;
static HBRUSH    g_bgBrush = nullptr;
static HICON     g_trayIcon = nullptr;
static NOTIFYICONDATAW g_nid = {};
static int       g_dpi = 96;
static bool      g_autoReapply = true;
static bool      g_autostartOn = false;
static bool      g_shownTrayHint = false;
static COLORREF  g_accent = RGB(0, 95, 184);       // Win11 default accent (light)
static COLORREF  g_accentPressed = RGB(0, 75, 150);

static PatchInfo    g_info = { PatchStatus::NvidiaNotFound, L"Checking..." };
static std::wstring g_lastCheck = L"";

// scale a 96-dpi metric to the current dpi
static int S(int v) { return MulDiv(v, g_dpi, 96); }

// ---------------------------------------------------------------- accent color
static COLORREF darken(COLORREF c, int amt) {
    int r = max(0, GetRValue(c) - amt), g = max(0, GetGValue(c) - amt), b = max(0, GetBValue(c) - amt);
    return RGB(r, g, b);
}
static void loadAccentColor() {
    // DWM stores the accent as 0xAABBGGRR under this key.
    HKEY k;
    if (RegOpenKeyExW(HKEY_CURRENT_USER, L"Software\\Microsoft\\Windows\\DWM", 0, KEY_READ, &k) == ERROR_SUCCESS) {
        DWORD val = 0, sz = sizeof(val), type = 0;
        if (RegQueryValueExW(k, L"AccentColor", nullptr, &type, (LPBYTE)&val, &sz) == ERROR_SUCCESS && type == REG_DWORD) {
            g_accent = RGB(val & 0xFF, (val >> 8) & 0xFF, (val >> 16) & 0xFF);
        }
        RegCloseKey(k);
    }
    g_accentPressed = darken(g_accent, 28);
}

// --------------------------------------------------------------- tray icon art
// Build a small round status "dot" icon at runtime (green/amber/grey/red),
// using a classic color+mask icon so no .ico resource is needed.
static HICON makeDotIcon(COLORREF fill, COLORREF ring) {
    const int N = 32;
    HDC screen = GetDC(nullptr);
    HDC dc = CreateCompatibleDC(screen);
    HBITMAP color = CreateCompatibleBitmap(screen, N, N);
    HBITMAP mask  = CreateBitmap(N, N, 1, 1, nullptr);

    HGDIOBJ oldC = SelectObject(dc, color);
    RECT rc = { 0, 0, N, N };
    FillRect(dc, &rc, (HBRUSH)GetStockObject(BLACK_BRUSH));
    HBRUSH fb = CreateSolidBrush(fill);
    HPEN   rp = CreatePen(PS_SOLID, 2, ring);
    HGDIOBJ ob = SelectObject(dc, fb), op = SelectObject(dc, rp);
    Ellipse(dc, 3, 3, N - 3, N - 3);
    SelectObject(dc, ob); SelectObject(dc, op);
    DeleteObject(fb); DeleteObject(rp);
    SelectObject(dc, oldC);

    HGDIOBJ oldM = SelectObject(dc, mask);
    PatBlt(dc, 0, 0, N, N, WHITENESS);
    HPEN mp = CreatePen(PS_SOLID, 1, RGB(0, 0, 0));
    HGDIOBJ omb = SelectObject(dc, GetStockObject(BLACK_BRUSH)), omp = SelectObject(dc, mp);
    Ellipse(dc, 3, 3, N - 3, N - 3);
    SelectObject(dc, omb); SelectObject(dc, omp);
    DeleteObject(mp);
    SelectObject(dc, oldM);

    ICONINFO ii = {}; ii.fIcon = TRUE; ii.hbmMask = mask; ii.hbmColor = color;
    HICON icon = CreateIconIndirect(&ii);
    DeleteObject(color); DeleteObject(mask); DeleteDC(dc); ReleaseDC(nullptr, screen);
    return icon;
}

struct Palette { COLORREF dot, ring; const wchar_t* label; };
static Palette paletteFor(PatchStatus s) {
    switch (s) {
        case PatchStatus::Patched:    return { RGB(46,160,67),  RGB(33,120,50),  L"Active" };
        case PatchStatus::NotPatched: return { RGB(232,160,40), RGB(190,130,25), L"Not applied" };
        case PatchStatus::Error:      return { RGB(210,70,70),  RGB(175,45,45),  L"Error" };
        case PatchStatus::NvidiaNotFound:
        default:                      return { RGB(150,150,150),RGB(120,120,120),L"NVIDIA not found" };
    }
}

// --------------------------------------------------------------- autostart
static std::wstring exePath() {
    wchar_t buf[MAX_PATH] = {};
    GetModuleFileNameW(nullptr, buf, MAX_PATH);
    return buf;
}
static bool autostartEnabled() {
    HKEY k;
    if (RegOpenKeyExW(HKEY_CURRENT_USER, kRunKey, 0, KEY_READ, &k) != ERROR_SUCCESS) return false;
    wchar_t buf[MAX_PATH + 16] = {};
    DWORD sz = sizeof(buf), type = 0;
    LONG r = RegQueryValueExW(k, kRunValue, nullptr, &type, (LPBYTE)buf, &sz);
    RegCloseKey(k);
    return r == ERROR_SUCCESS && type == REG_SZ && buf[0] != 0;
}
static void setAutostart(bool on) {
    HKEY k;
    if (RegCreateKeyExW(HKEY_CURRENT_USER, kRunKey, 0, nullptr, 0, KEY_SET_VALUE, nullptr, &k, nullptr) != ERROR_SUCCESS)
        return;
    if (on) {
        std::wstring cmd = L"\"" + exePath() + L"\" --minimized";
        RegSetValueExW(k, kRunValue, 0, REG_SZ, (const BYTE*)cmd.c_str(),
                       (DWORD)((cmd.size() + 1) * sizeof(wchar_t)));
    } else {
        RegDeleteValueW(k, kRunValue);
    }
    RegCloseKey(k);
}

// ------------------------------------------------------------------ tray
static void trayAdd() {
    g_nid.cbSize = sizeof(g_nid);
    g_nid.hWnd = g_hwnd;
    g_nid.uID = 1;
    g_nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    g_nid.uCallbackMessage = WM_TRAYICON;
    g_nid.hIcon = g_trayIcon;
    wcscpy_s(g_nid.szTip, kAppName);
    Shell_NotifyIconW(NIM_ADD, &g_nid);
}
static void trayUpdate(const std::wstring& tip, HICON icon) {
    g_nid.uFlags = NIF_ICON | NIF_TIP;
    g_nid.hIcon = icon;
    wcsncpy_s(g_nid.szTip, tip.c_str(), _TRUNCATE);
    Shell_NotifyIconW(NIM_MODIFY, &g_nid);
}
static void trayBalloon(const std::wstring& title, const std::wstring& text) {
    g_nid.uFlags = NIF_INFO;
    wcsncpy_s(g_nid.szInfoTitle, title.c_str(), _TRUNCATE);
    wcsncpy_s(g_nid.szInfo, text.c_str(), _TRUNCATE);
    g_nid.dwInfoFlags = NIIF_INFO;
    Shell_NotifyIconW(NIM_MODIFY, &g_nid);
}
static void trayRemove() { Shell_NotifyIconW(NIM_DELETE, &g_nid); }

// --------------------------------------------------------------- state refresh
static std::wstring nowString() {
    SYSTEMTIME st; GetLocalTime(&st);
    wchar_t b[32];
    swprintf_s(b, L"%02d:%02d:%02d", st.wHour, st.wMinute, st.wSecond);
    return b;
}
static void syncTrayIconToStatus() {
    Palette p = paletteFor(g_info.status);
    HICON old = g_trayIcon;
    g_trayIcon = makeDotIcon(p.dot, p.ring);
    trayUpdate(std::wstring(kAppName) + L" - " + p.label, g_trayIcon);
    SendMessageW(g_hwnd, WM_SETICON, ICON_SMALL, (LPARAM)g_trayIcon);
    SendMessageW(g_hwnd, WM_SETICON, ICON_BIG, (LPARAM)g_trayIcon);
    if (old) DestroyIcon(old);
    InvalidateRect(g_hwnd, nullptr, FALSE);
}
static void refreshState(bool allowApply) {
    g_info = queryShadowPlayStatus();
    if (allowApply && g_autoReapply && g_info.status == PatchStatus::NotPatched)
        g_info = applyShadowPlayPatch();
    g_lastCheck = nowString();
    syncTrayIconToStatus();
}

// ------------------------------------------------------------------ drawing
static void fillRoundRect(HDC dc, RECT rc, int radius, COLORREF fill, COLORREF edge, int penW) {
    HBRUSH b = CreateSolidBrush(fill);
    HPEN   p = CreatePen(PS_SOLID, penW, edge);
    HGDIOBJ ob = SelectObject(dc, b), op = SelectObject(dc, p);
    RoundRect(dc, rc.left, rc.top, rc.right, rc.bottom, radius, radius);
    SelectObject(dc, ob); SelectObject(dc, op);
    DeleteObject(b); DeleteObject(p);
}

static void paintCard(HDC hdc, const RECT& client) {
    Palette p = paletteFor(g_info.status);
    FillRect(hdc, &client, g_bgBrush);
    SetBkMode(hdc, TRANSPARENT);
    const int m = S(20);

    // header + subtitle (distinct hierarchy, not a raw duplicate of the title bar)
    HGDIOBJ of = SelectObject(hdc, g_headerFont);
    SetTextColor(hdc, kText);
    RECT rh = { m, S(16), client.right - m, S(44) };
    DrawTextW(hdc, kAppName, -1, &rh, DT_LEFT | DT_SINGLELINE);

    SelectObject(hdc, g_subFont);
    SetTextColor(hdc, kSubText);
    RECT rs = { m, S(42), client.right - m, S(62) };
    DrawTextW(hdc, L"Keeps NVIDIA Instant Replay from turning itself off.", -1, &rs, DT_LEFT | DT_SINGLELINE);

    // status card (Mica-ish surface: soft fill + hairline border, 8px radius)
    RECT card = { m, S(72), client.right - m, S(72) + S(56) };
    fillRoundRect(hdc, card, S(16), kCard, kCardEdge, 1);

    // small status dot
    int cy = (card.top + card.bottom) / 2;
    int dr = S(6);
    int dx = card.left + S(18);
    HBRUSH db = CreateSolidBrush(p.dot);
    HPEN   dpn = CreatePen(PS_SOLID, S(1), p.ring);
    HGDIOBJ ob = SelectObject(hdc, db), op = SelectObject(hdc, dpn);
    Ellipse(hdc, dx - dr, cy - dr, dx + dr, cy + dr);
    SelectObject(hdc, ob); SelectObject(hdc, op);
    DeleteObject(db); DeleteObject(dpn);

    // status label
    SelectObject(hdc, g_statusFont);
    SetTextColor(hdc, kText);
    RECT rl = { dx + dr + S(14), card.top, card.right - S(14), card.bottom };
    DrawTextW(hdc, p.label, -1, &rl, DT_LEFT | DT_SINGLELINE | DT_VCENTER);

    // detail message under the card
    SelectObject(hdc, g_bodyFont);
    SetTextColor(hdc, kSubText);
    RECT rd = { m, card.bottom + S(10), client.right - m, card.bottom + S(48) };
    DrawTextW(hdc, g_info.message.c_str(), -1, &rd, DT_LEFT | DT_WORDBREAK);

    // footer: last checked
    if (!g_lastCheck.empty()) {
        SelectObject(hdc, g_smallFont);
        SetTextColor(hdc, kFaint);
        std::wstring foot = L"Last checked " + g_lastCheck;
        RECT rf = { m, client.bottom - S(26), client.right - m, client.bottom - S(6) };
        DrawTextW(hdc, foot.c_str(), -1, &rf, DT_LEFT | DT_SINGLELINE);
    }
    SelectObject(hdc, of);
}

static void drawPrimaryButton(const DRAWITEMSTRUCT* d) {
    HDC dc = d->hDC;
    RECT rc = d->rcItem;
    FillRect(dc, &rc, g_bgBrush);
    bool pressed = (d->itemState & ODS_SELECTED) != 0;
    COLORREF c = pressed ? g_accentPressed : g_accent;
    fillRoundRect(dc, rc, S(8), c, c, 1); // ~4px corners
    SetBkMode(dc, TRANSPARENT);
    SelectObject(dc, g_bodyFont);
    SetTextColor(dc, RGB(255, 255, 255));
    DrawTextW(dc, L"Patch now", -1, &rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
}

static void drawToggle(const DRAWITEMSTRUCT* d, bool on, const wchar_t* text) {
    HDC dc = d->hDC;
    RECT rc = d->rcItem;
    FillRect(dc, &rc, g_bgBrush);
    SetBkMode(dc, TRANSPARENT);

    int h = rc.bottom - rc.top;
    int trackW = S(40), trackH = S(20);
    int ty = rc.top + (h - trackH) / 2;
    int tx = rc.left;

    COLORREF track = on ? g_accent : kTglOffTrk;
    COLORREF edge  = on ? g_accent : kTglOffEdge;
    RECT tr = { tx, ty, tx + trackW, ty + trackH };
    fillRoundRect(dc, tr, trackH, track, edge, S(1));

    int pad = S(3), kd = trackH - 2 * pad;
    int kx = on ? (tx + trackW - pad - kd) : (tx + pad);
    COLORREF knob = on ? RGB(255, 255, 255) : kTglOffKnob;
    HBRUSH kb = CreateSolidBrush(knob);
    HPEN   kp = CreatePen(PS_SOLID, 1, knob);
    HGDIOBJ ob = SelectObject(dc, kb), op = SelectObject(dc, kp);
    Ellipse(dc, kx, ty + pad, kx + kd, ty + pad + kd);
    SelectObject(dc, ob); SelectObject(dc, op);
    DeleteObject(kb); DeleteObject(kp);

    SelectObject(dc, g_bodyFont);
    SetTextColor(dc, kText);
    RECT rt = { tx + trackW + S(12), rc.top, rc.right, rc.bottom };
    DrawTextW(dc, text, -1, &rt, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
}

// ------------------------------------------------------------------ window
static void showWindow() { ShowWindow(g_hwnd, SW_SHOW); SetForegroundWindow(g_hwnd); }

static void showContextMenu() {
    HMENU menu = CreatePopupMenu();
    AppendMenuW(menu, MF_STRING, IDM_OPEN, L"Open");
    AppendMenuW(menu, MF_STRING, IDM_PATCH, L"Patch now");
    AppendMenuW(menu, MF_SEPARATOR, 0, nullptr);
    AppendMenuW(menu, MF_STRING | (g_autostartOn ? MF_CHECKED : 0), IDM_AUTOSTART, L"Run at startup");
    AppendMenuW(menu, MF_STRING | (g_autoReapply ? MF_CHECKED : 0), IDM_REAPPLY, L"Auto re-apply");
    AppendMenuW(menu, MF_SEPARATOR, 0, nullptr);
    AppendMenuW(menu, MF_STRING, IDM_QUIT, L"Quit");

    POINT pt; GetCursorPos(&pt);
    SetForegroundWindow(g_hwnd);
    TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, 0, g_hwnd, nullptr);
    PostMessageW(g_hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);
}

static void doPatchNow() {
    g_info = applyShadowPlayPatch();
    g_lastCheck = nowString();
    syncTrayIconToStatus();
    trayBalloon(kAppName, g_info.message);
}

static LRESULT CALLBACK WndProc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp) {
    switch (msg) {
    case WM_CREATE: {
        g_dpi = (int)GetDpiForWindow(hwnd);
        auto mkFont = [](const wchar_t* face, int pt, int weight) {
            return CreateFontW(-MulDiv(pt, g_dpi, 72), 0, 0, 0, weight, FALSE, FALSE, FALSE,
                DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY, DEFAULT_PITCH | FF_DONTCARE, face);
        };
        g_headerFont = mkFont(L"Segoe UI Variable Display", 15, FW_SEMIBOLD);
        g_subFont    = mkFont(L"Segoe UI Variable Text", 9, FW_NORMAL);
        g_statusFont = mkFont(L"Segoe UI Variable Text", 12, FW_SEMIBOLD);
        g_bodyFont   = mkFont(L"Segoe UI Variable Text", 10, FW_NORMAL);
        g_smallFont  = mkFont(L"Segoe UI Variable Small", 9, FW_NORMAL);

        g_autostartOn = autostartEnabled();

        g_btnPatch = CreateWindowExW(0, L"BUTTON", L"Patch now",
            WS_CHILD | WS_VISIBLE | BS_OWNERDRAW,
            S(20), S(184), S(134), S(38), hwnd, (HMENU)ID_BTN_PATCH, g_hInst, nullptr);
        g_tglAutostart = CreateWindowExW(0, L"BUTTON", L"Run when Windows starts",
            WS_CHILD | WS_VISIBLE | BS_OWNERDRAW,
            S(20), S(238), S(340), S(28), hwnd, (HMENU)ID_TGL_AUTOSTART, g_hInst, nullptr);
        g_tglReapply = CreateWindowExW(0, L"BUTTON", L"Auto re-apply when NVIDIA restarts",
            WS_CHILD | WS_VISIBLE | BS_OWNERDRAW,
            S(20), S(272), S(340), S(28), hwnd, (HMENU)ID_TGL_REAPPLY, g_hInst, nullptr);
        return 0;
    }

    case WM_DRAWITEM: {
        const DRAWITEMSTRUCT* d = (const DRAWITEMSTRUCT*)lp;
        if (d->CtlID == ID_BTN_PATCH)          drawPrimaryButton(d);
        else if (d->CtlID == ID_TGL_AUTOSTART) drawToggle(d, g_autostartOn, L"Run when Windows starts");
        else if (d->CtlID == ID_TGL_REAPPLY)   drawToggle(d, g_autoReapply, L"Auto re-apply when NVIDIA restarts");
        return TRUE;
    }

    case WM_COMMAND: {
        switch (LOWORD(wp)) {
        case ID_BTN_PATCH:
        case IDM_PATCH:
            doPatchNow();
            return 0;
        case IDM_OPEN:
            showWindow();
            return 0;
        case ID_TGL_AUTOSTART:
            g_autostartOn = !g_autostartOn;
            setAutostart(g_autostartOn);
            InvalidateRect(g_tglAutostart, nullptr, FALSE);
            return 0;
        case IDM_AUTOSTART:
            g_autostartOn = !g_autostartOn;
            setAutostart(g_autostartOn);
            InvalidateRect(g_tglAutostart, nullptr, FALSE);
            return 0;
        case ID_TGL_REAPPLY:
            g_autoReapply = !g_autoReapply;
            InvalidateRect(g_tglReapply, nullptr, FALSE);
            return 0;
        case IDM_REAPPLY:
            g_autoReapply = !g_autoReapply;
            InvalidateRect(g_tglReapply, nullptr, FALSE);
            return 0;
        case IDM_QUIT:
            DestroyWindow(hwnd);
            return 0;
        }
        return 0;
    }

    case WM_TRAYICON:
        switch (LOWORD(lp)) {
        case WM_LBUTTONDBLCLK: showWindow(); break;
        case WM_RBUTTONUP:
        case WM_CONTEXTMENU:   showContextMenu(); break;
        }
        return 0;

    case WM_TIMER:
        if (wp == TIMER_POLL) refreshState(true);
        return 0;

    case WM_PAINT: {
        PAINTSTRUCT ps;
        HDC hdc = BeginPaint(hwnd, &ps);
        RECT client; GetClientRect(hwnd, &client);
        HDC mem = CreateCompatibleDC(hdc);
        HBITMAP bmp = CreateCompatibleBitmap(hdc, client.right, client.bottom);
        HGDIOBJ oldb = SelectObject(mem, bmp);
        paintCard(mem, client);
        BitBlt(hdc, 0, 0, client.right, client.bottom, mem, 0, 0, SRCCOPY);
        SelectObject(mem, oldb);
        DeleteObject(bmp);
        DeleteDC(mem);
        EndPaint(hwnd, &ps);
        return 0;
    }

    case WM_CLOSE:
        ShowWindow(hwnd, SW_HIDE);
        if (!g_shownTrayHint) {
            g_shownTrayHint = true;
            trayBalloon(kAppName, L"Still running here. Right-click the icon for options, or double-click to reopen.");
        }
        return 0;

    case WM_DESTROY:
        KillTimer(hwnd, TIMER_POLL);
        trayRemove();
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcW(hwnd, msg, wp, lp);
}

int WINAPI wWinMain(HINSTANCE hInst, HINSTANCE, LPWSTR, int) {
    HANDLE mtx = CreateMutexW(nullptr, FALSE, kMutexName);
    if (mtx && GetLastError() == ERROR_ALREADY_EXISTS) {
        HWND existing = FindWindowW(kWndClass, nullptr);
        if (existing) { ShowWindow(existing, SW_SHOW); SetForegroundWindow(existing); }
        return 0;
    }

    // --no-apply: observe-only (never auto-patch; the "Patch now" button still works).
    if (wcsstr(GetCommandLineW(), L"--no-apply") != nullptr) g_autoReapply = false;

    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_SYSTEM_AWARE);
    g_hInst = hInst;
    loadAccentColor();
    INITCOMMONCONTROLSEX icc = { sizeof(icc), ICC_STANDARD_CLASSES };
    InitCommonControlsEx(&icc);

    g_bgBrush = CreateSolidBrush(kBg);
    g_trayIcon = makeDotIcon(RGB(150, 150, 150), RGB(120, 120, 120));

    WNDCLASSEXW wc = { sizeof(wc) };
    wc.lpfnWndProc = WndProc;
    wc.hInstance = hInst;
    wc.hCursor = LoadCursor(nullptr, IDC_ARROW);
    wc.hbrBackground = g_bgBrush;
    wc.lpszClassName = kWndClass;
    wc.hIcon = g_trayIcon;
    RegisterClassExW(&wc);

    g_dpi = (int)GetDpiForSystem();
    RECT r = { 0, 0, S(400), S(340) };
    DWORD style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
    AdjustWindowRectEx(&r, style, FALSE, 0);

    g_hwnd = CreateWindowExW(0, kWndClass, kAppName, style,
        CW_USEDEFAULT, CW_USEDEFAULT, r.right - r.left, r.bottom - r.top,
        nullptr, nullptr, hInst, nullptr);

    trayAdd();
    refreshState(true);
    SetTimer(g_hwnd, TIMER_POLL, POLL_MS, nullptr);

    bool startMinimized = wcsstr(GetCommandLineW(), L"--minimized") != nullptr;
    ShowWindow(g_hwnd, startMinimized ? SW_HIDE : SW_SHOW);
    UpdateWindow(g_hwnd);

    MSG msg;
    while (GetMessageW(&msg, nullptr, 0, 0) > 0) {
        if (IsDialogMessageW(g_hwnd, &msg)) continue;
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

    if (g_trayIcon) DestroyIcon(g_trayIcon);
    if (g_bgBrush) DeleteObject(g_bgBrush);
    if (mtx) CloseHandle(mtx);
    return 0;
}
