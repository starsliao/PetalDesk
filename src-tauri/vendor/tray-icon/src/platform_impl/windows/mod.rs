// Copyright 2022-2022 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

mod icon;
mod util;
use std::{mem::size_of, ptr};

use once_cell::sync::Lazy;
use windows_sys::{
    s,
    Win32::{
        Foundation::{FALSE, HWND, LPARAM, LRESULT, POINT, TRUE, WPARAM},
        UI::{
            Shell::{
                Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_STATE, NIF_TIP, NIM_ADD, NIM_DELETE,
                NIM_MODIFY, NIS_HIDDEN, NOTIFYICONDATAW,
            },
            WindowsAndMessaging::{
                ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, DestroyWindow,
                GetCursorPos, KillTimer, PostMessageW, RegisterClassW, RegisterWindowMessageA,
                SendMessageW, SetForegroundWindow, SetTimer, TrackPopupMenu, CREATESTRUCTW,
                CW_USEDEFAULT, GWL_USERDATA, HICON, HMENU, MSGFLT_ALLOW, TPM_BOTTOMALIGN,
                TPM_LEFTALIGN, WM_CREATE, WM_DESTROY, WM_DEVICECHANGE, WM_DISPLAYCHANGE,
                WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDBLCLK, WM_MBUTTONDOWN,
                WM_MBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE, WM_NULL, WM_RBUTTONDBLCLK, WM_RBUTTONDOWN,
                WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
                WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_OVERLAPPED,
            },
        },
    },
};

use crate::{
    dpi::PhysicalPosition, icon::Icon, menu, MouseButton, MouseButtonState, Rect,
    TrayIconAttributes, TrayIconEvent, TrayIconId, COUNTER,
};

pub(crate) use self::icon::WinIcon as PlatformIcon;

const WM_USER_TRAYICON: u32 = 6002;
const WM_USER_UPDATE_TRAYMENU: u32 = 6003;
const WM_USER_UPDATE_TRAYICON: u32 = 6004;
const WM_USER_SHOW_TRAYICON: u32 = 6005;
const WM_USER_UPDATE_TRAYTOOLTIP: u32 = 6006;
const WM_USER_LEAVE_TIMER_ID: u32 = 6007;
const WM_USER_SHOW_MENU_ON_LEFT_CLICK: u32 = 6008;
const WM_USER_SHOW_MENU_ON_RIGHT_CLICK: u32 = 6009;
const LEAVE_TIMER_INTERVAL_MS: u32 = 100;
/// When the taskbar is created, it registers a message with the "TaskbarCreated" string and then broadcasts this message to all top-level windows
/// When the application receives this message, it should assume that any taskbar icons it added have been removed and add them again.
static S_U_TASKBAR_RESTART: Lazy<u32> =
    Lazy::new(|| unsafe { RegisterWindowMessageA(s!("TaskbarCreated")) });

#[derive(Debug, Default)]
struct HoverState {
    entered: bool,
    last_position: Option<PhysicalPosition<f64>>,
}

#[derive(Debug, PartialEq)]
enum HoverEvent {
    Enter,
    Move,
    None,
}

impl HoverState {
    fn update(&mut self, position: PhysicalPosition<f64>) -> HoverEvent {
        let event = if !self.entered {
            self.entered = true;
            HoverEvent::Enter
        } else if self.last_position != Some(position) {
            HoverEvent::Move
        } else {
            HoverEvent::None
        };
        self.last_position = Some(position);
        event
    }

    fn leave_if_position_changed(
        &mut self,
        current_position: PhysicalPosition<f64>,
    ) -> Option<PhysicalPosition<f64>> {
        let last_position = self.last_position?;
        if last_position == current_position {
            return None;
        }
        self.reset();
        Some(last_position)
    }

    fn reset(&mut self) {
        self.entered = false;
        self.last_position = None;
    }
}

fn cursor_anchor_rect(position: PhysicalPosition<f64>) -> Rect {
    Rect {
        position,
        size: crate::dpi::PhysicalSize::new(1, 1),
    }
}

struct TrayUserData {
    internal_id: u32,
    id: TrayIconId,
    hwnd: HWND,
    hpopupmenu: Option<HMENU>,
    icon: Option<Icon>,
    tooltip: Option<String>,
    hover: HoverState,
    menu_on_left_click: bool,
    menu_on_right_click: bool,
    visible: bool,
}

impl TrayUserData {
    fn set_tray_visible(&mut self, visible: bool) {
        self.visible = visible;
        let nid = NOTIFYICONDATAW {
            uFlags: NIF_STATE,
            hWnd: self.hwnd,
            uID: self.internal_id,
            dwState: if visible { 0 } else { NIS_HIDDEN },
            dwStateMask: NIS_HIDDEN,
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            ..Default::default()
        };
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
    }
}

pub struct TrayIcon {
    hwnd: HWND,
    menu: Option<Box<dyn menu::ContextMenu>>,
    internal_id: u32,
}

impl TrayIcon {
    pub fn new(id: TrayIconId, attrs: TrayIconAttributes) -> crate::Result<Self> {
        let internal_id = COUNTER.next();

        let class_name = util::encode_wide("tray_icon_app");
        unsafe {
            let hinstance = util::get_instance_handle();

            let wnd_class = WNDCLASSW {
                lpfnWndProc: Some(tray_proc),
                lpszClassName: class_name.as_ptr(),
                hInstance: hinstance,
                ..std::mem::zeroed()
            };

            RegisterClassW(&wnd_class);

            let traydata = TrayUserData {
                id,
                internal_id,
                hwnd: std::ptr::null_mut(),
                hpopupmenu: attrs.menu.as_ref().map(|m| m.hpopupmenu() as _),
                icon: attrs.icon.clone(),
                tooltip: attrs.tooltip.clone(),
                hover: HoverState::default(),
                menu_on_left_click: attrs.menu_on_left_click,
                menu_on_right_click: attrs.menu_on_right_click,
                visible: true,
            };

            let hwnd = CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED |
            // WS_EX_TOOLWINDOW prevents this window from ever showing up in the taskbar, which
            // we want to avoid. If you remove this style, this window won't show up in the
            // taskbar *initially*, but it can show up at some later point. This can sometimes
            // happen on its own after several hours have passed, although this has proven
            // difficult to reproduce. Alternatively, it can be manually triggered by killing
            // `explorer.exe` and then starting the process back up.
            // It is unclear why the bug is triggered by waiting for several hours.
            WS_EX_TOOLWINDOW,
                class_name.as_ptr(),
                ptr::null(),
                WS_OVERLAPPED,
                CW_USEDEFAULT,
                0,
                CW_USEDEFAULT,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinstance,
                Box::into_raw(Box::new(traydata)) as _,
            );
            if hwnd.is_null() {
                return Err(crate::Error::OsError(std::io::Error::last_os_error()));
            }

            // Allow "TaskbarCreated" through UIPI so elevated apps can re-register on explorer restart.
            ChangeWindowMessageFilterEx(hwnd, *S_U_TASKBAR_RESTART, MSGFLT_ALLOW, ptr::null_mut());

            let hicon = attrs.icon.as_ref().map(|i| i.inner.as_raw_handle());

            if !register_tray_icon(hwnd, internal_id, &hicon, &attrs.tooltip, true) {
                // Explorer/taskbar may not be ready yet (e.g., app starts before explorer.exe).
                // Keep the window alive and wait for TaskbarCreated to re-register.
            }

            if let Some(menu) = &attrs.menu {
                menu.attach_menu_subclass_for_hwnd(hwnd as _);
            }

            Ok(Self {
                hwnd,
                internal_id,
                menu: attrs.menu,
            })
        }
    }

    pub fn set_icon(&mut self, icon: Option<Icon>) -> crate::Result<()> {
        unsafe {
            let mut nid = NOTIFYICONDATAW {
                uFlags: NIF_ICON,
                hWnd: self.hwnd,
                uID: self.internal_id,
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                ..std::mem::zeroed()
            };

            if let Some(hicon) = icon.as_ref().map(|i| i.inner.as_raw_handle()) {
                nid.hIcon = hicon;
            }

            if Shell_NotifyIconW(NIM_MODIFY, &nid) == 0 {
                return Err(crate::Error::OsError(std::io::Error::last_os_error()));
            }

            // send the new icon to the subclass proc to store it in the tray data
            SendMessageW(
                self.hwnd,
                WM_USER_UPDATE_TRAYICON,
                Box::into_raw(Box::new(icon)) as _,
                0,
            );
        }

        Ok(())
    }

    pub fn set_menu(&mut self, menu: Option<Box<dyn menu::ContextMenu>>) {
        // Safety: self.hwnd is valid as long as as the TrayIcon is
        if let Some(menu) = &self.menu {
            unsafe { menu.detach_menu_subclass_from_hwnd(self.hwnd as _) };
        }
        if let Some(menu) = &menu {
            unsafe { menu.attach_menu_subclass_for_hwnd(self.hwnd as _) };
        }

        unsafe {
            // send the new menu to the subclass proc where we will update there
            SendMessageW(
                self.hwnd,
                WM_USER_UPDATE_TRAYMENU,
                Box::into_raw(Box::new(menu.as_ref().map(|m| m.hpopupmenu()))) as _,
                0,
            );
        }

        self.menu = menu;
    }

    pub fn set_tooltip<S: AsRef<str>>(&mut self, tooltip: Option<S>) -> crate::Result<()> {
        unsafe {
            let mut nid = NOTIFYICONDATAW {
                uFlags: NIF_TIP,
                hWnd: self.hwnd,
                uID: self.internal_id,
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                ..std::mem::zeroed()
            };
            if let Some(tooltip) = &tooltip {
                let tip = util::encode_wide(tooltip.as_ref());
                #[allow(clippy::manual_memcpy)]
                for i in 0..tip.len().min(128) {
                    nid.szTip[i] = tip[i];
                }
            }

            if Shell_NotifyIconW(NIM_MODIFY, &nid) == 0 {
                return Err(crate::Error::OsError(std::io::Error::last_os_error()));
            }

            // send the new tooltip to the subclass proc to store it in the tray data
            SendMessageW(
                self.hwnd,
                WM_USER_UPDATE_TRAYTOOLTIP,
                Box::into_raw(Box::new(tooltip.map(|t| t.as_ref().to_string()))) as _,
                0,
            );
        }

        Ok(())
    }

    pub fn set_show_menu_on_left_click(&mut self, enable: bool) {
        unsafe {
            SendMessageW(
                self.hwnd,
                WM_USER_SHOW_MENU_ON_LEFT_CLICK,
                enable as usize,
                0,
            );
        }
    }

    pub fn set_show_menu_on_right_click(&mut self, enable: bool) {
        unsafe {
            SendMessageW(
                self.hwnd,
                WM_USER_SHOW_MENU_ON_RIGHT_CLICK,
                enable as usize,
                0,
            );
        }
    }

    pub fn show_menu(&self) {
        if let Some(menu) = &self.menu {
            unsafe {
                let mut cursor = POINT { x: 0, y: 0 };
                if GetCursorPos(&mut cursor as _) != 0 {
                    show_tray_menu(self.hwnd, menu.hpopupmenu() as _, cursor.x, cursor.y);
                }
            }
        }
    }

    pub fn set_title<S: AsRef<str>>(&mut self, _title: Option<S>) {}

    pub fn set_visible(&mut self, visible: bool) -> crate::Result<()> {
        unsafe {
            SendMessageW(self.hwnd, WM_USER_SHOW_TRAYICON, visible as usize, 0);
        }
        Ok(())
    }

    pub fn rect(&self) -> Option<Rect> {
        // The Windows notification-area rectangle query can synchronously wait
        // on Explorer and deadlock the UI thread during RDP transitions. There
        // is no exact non-blocking replacement for this optional API.
        None
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        unsafe {
            remove_tray_icon(self.hwnd, self.internal_id);

            if let Some(menu) = &self.menu {
                menu.detach_menu_subclass_from_hwnd(self.hwnd as _);
            }

            // destroy the hidden window used by the tray
            DestroyWindow(self.hwnd);
        }
    }
}

unsafe extern "system" fn tray_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let userdata_ptr = unsafe { util::get_window_long(hwnd, GWL_USERDATA) };
    let userdata_ptr = match (userdata_ptr, msg) {
        (0, WM_NCCREATE) => {
            let createstruct = unsafe { &mut *(lparam as *mut CREATESTRUCTW) };
            let userdata = unsafe { &mut *(createstruct.lpCreateParams as *mut TrayUserData) };
            userdata.hwnd = hwnd;
            util::set_window_long(hwnd, GWL_USERDATA, createstruct.lpCreateParams as _);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        // Getting here should quite frankly be impossible,
        // but we'll make window creation fail here just in case.
        (0, WM_CREATE) => return -1,
        (_, WM_CREATE) => return DefWindowProcW(hwnd, msg, wparam, lparam),
        (0, _) => return DefWindowProcW(hwnd, msg, wparam, lparam),
        _ => userdata_ptr as *mut TrayUserData,
    };

    let userdata = &mut *(userdata_ptr);

    match msg {
        WM_DESTROY => {
            KillTimer(hwnd, WM_USER_LEAVE_TIMER_ID as _);
            drop(Box::from_raw(userdata_ptr));
            return 0;
        }
        WM_USER_UPDATE_TRAYMENU => {
            let hpopupmenu = Box::from_raw(wparam as *mut Option<isize>);
            userdata.hpopupmenu = (*hpopupmenu).map(|h| h as *mut _);
        }
        WM_USER_UPDATE_TRAYICON => {
            let icon = Box::from_raw(wparam as *mut Option<Icon>);
            userdata.icon = *icon;
        }
        WM_USER_SHOW_TRAYICON => {
            let visible = wparam as i32 == TRUE;
            if !visible {
                KillTimer(hwnd, WM_USER_LEAVE_TIMER_ID as _);
                userdata.hover.reset();
            }
            userdata.set_tray_visible(visible);
        }
        WM_USER_UPDATE_TRAYTOOLTIP => {
            let tooltip = Box::from_raw(wparam as *mut Option<String>);
            userdata.tooltip = *tooltip;
        }
        _ if msg == *S_U_TASKBAR_RESTART => {
            KillTimer(hwnd, WM_USER_LEAVE_TIMER_ID as _);
            userdata.hover.reset();
            remove_tray_icon(userdata.hwnd, userdata.internal_id);
            register_tray_icon(
                userdata.hwnd,
                userdata.internal_id,
                &userdata.icon.as_ref().map(|i| i.inner.as_raw_handle()),
                &userdata.tooltip,
                userdata.visible,
            );
        }
        WM_DEVICECHANGE | WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
            // RDP reconnects and display/device changes invalidate the hover
            // sequence. Reset only local state; never pump nested messages
            // from inside this window procedure.
            KillTimer(hwnd, WM_USER_LEAVE_TIMER_ID as _);
            userdata.hover.reset();
        }
        WM_USER_SHOW_MENU_ON_LEFT_CLICK => {
            userdata.menu_on_left_click = wparam != 0;
        }
        WM_USER_SHOW_MENU_ON_RIGHT_CLICK => {
            userdata.menu_on_right_click = wparam != 0;
        }

        WM_USER_TRAYICON
            if matches!(
                lparam as u32,
                WM_LBUTTONDOWN
                    | WM_RBUTTONDOWN
                    | WM_MBUTTONDOWN
                    | WM_LBUTTONUP
                    | WM_RBUTTONUP
                    | WM_MBUTTONUP
                    | WM_LBUTTONDBLCLK
                    | WM_RBUTTONDBLCLK
                    | WM_MBUTTONDBLCLK
                    | WM_MOUSEMOVE
            ) =>
        {
            let mut cursor = POINT { x: 0, y: 0 };
            if GetCursorPos(&mut cursor as _) == 0 {
                return 0;
            }

            let id = userdata.id.clone();
            let position = PhysicalPosition::new(cursor.x as f64, cursor.y as f64);
            let rect = cursor_anchor_rect(position);

            let event = match lparam as u32 {
                WM_LBUTTONDOWN => TrayIconEvent::Click {
                    id,
                    rect,
                    position,
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Down,
                },
                WM_RBUTTONDOWN => TrayIconEvent::Click {
                    id,
                    rect,
                    position,
                    button: MouseButton::Right,
                    button_state: MouseButtonState::Down,
                },
                WM_MBUTTONDOWN => TrayIconEvent::Click {
                    id,
                    rect,
                    position,
                    button: MouseButton::Middle,
                    button_state: MouseButtonState::Down,
                },
                WM_LBUTTONUP => TrayIconEvent::Click {
                    id,
                    rect,
                    position,
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                },
                WM_RBUTTONUP => TrayIconEvent::Click {
                    id,
                    rect,
                    position,
                    button: MouseButton::Right,
                    button_state: MouseButtonState::Up,
                },
                WM_MBUTTONUP => TrayIconEvent::Click {
                    id,
                    rect,
                    position,
                    button: MouseButton::Middle,
                    button_state: MouseButtonState::Up,
                },
                WM_LBUTTONDBLCLK => TrayIconEvent::DoubleClick {
                    id,
                    rect,
                    position,
                    button: MouseButton::Left,
                },
                WM_RBUTTONDBLCLK => TrayIconEvent::DoubleClick {
                    id,
                    rect,
                    position,
                    button: MouseButton::Right,
                },
                WM_MBUTTONDBLCLK => TrayIconEvent::DoubleClick {
                    id,
                    rect,
                    position,
                    button: MouseButton::Middle,
                },
                WM_MOUSEMOVE => {
                    // A timer with an existing ID is replaced and restarted.
                    // WM_TIMER is low priority, so queued tray moves update the
                    // last position before we decide that the cursor left.
                    SetTimer(
                        hwnd,
                        WM_USER_LEAVE_TIMER_ID as _,
                        LEAVE_TIMER_INTERVAL_MS,
                        Some(tray_timer_proc),
                    );
                    match userdata.hover.update(position) {
                        HoverEvent::Enter => TrayIconEvent::Enter { id, rect, position },
                        HoverEvent::Move => TrayIconEvent::Move { id, rect, position },
                        HoverEvent::None => return 0,
                    }
                }

                _ => unreachable!(),
            };

            TrayIconEvent::send(event);

            if (userdata.menu_on_right_click && lparam as u32 == WM_RBUTTONUP)
                || (userdata.menu_on_left_click && lparam as u32 == WM_LBUTTONUP)
            {
                if let Some(menu) = userdata.hpopupmenu {
                    show_tray_menu(hwnd, menu, cursor.x, cursor.y);
                }
            }
        }

        WM_TIMER if wparam as u32 == WM_USER_LEAVE_TIMER_ID => {
            let mut cursor = POINT { x: 0, y: 0 };
            if GetCursorPos(&mut cursor as _) == 0 {
                return 0;
            }
            let current_position = PhysicalPosition::new(cursor.x as f64, cursor.y as f64);
            if let Some(position) = userdata.hover.leave_if_position_changed(current_position) {
                KillTimer(hwnd, WM_USER_LEAVE_TIMER_ID as _);
                TrayIconEvent::send(TrayIconEvent::Leave {
                    id: userdata.id.clone(),
                    rect: cursor_anchor_rect(position),
                    position,
                });
            }

            return 0;
        }

        _ => {}
    }

    DefWindowProcW(hwnd, msg, wparam, lparam)
}

unsafe extern "system" fn tray_timer_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: u32) {
    tray_proc(hwnd, msg, wparam, lparam as _);
}

#[inline]
unsafe fn show_tray_menu(hwnd: HWND, menu: HMENU, x: i32, y: i32) {
    // bring the hidden window to the foreground so the pop up menu
    // would automatically hide on click outside
    SetForegroundWindow(hwnd);
    TrackPopupMenu(
        menu,
        // align bottom / right, maybe we could expose this later..
        TPM_BOTTOMALIGN | TPM_LEFTALIGN,
        x,
        y,
        0,
        hwnd,
        std::ptr::null_mut(),
    );
    // The shell docs recommend posting a benign message after TrackPopupMenu
    // for notification area menus so the task switch is finalized correctly.
    PostMessageW(hwnd, WM_NULL, 0, 0);
}

#[inline]
unsafe fn register_tray_icon(
    hwnd: HWND,
    tray_id: u32,
    hicon: &Option<HICON>,
    tooltip: &Option<String>,
    visible: bool,
) -> bool {
    let mut h_icon = std::ptr::null_mut();
    let mut flags = NIF_MESSAGE;
    let mut sz_tip: [u16; 128] = [0; 128];

    if let Some(hicon) = hicon {
        flags |= NIF_ICON;
        h_icon = *hicon;
    }

    if let Some(tooltip) = tooltip {
        flags |= NIF_TIP;
        let tip = util::encode_wide(tooltip);
        #[allow(clippy::manual_memcpy)]
        for i in 0..tip.len().min(128) {
            sz_tip[i] = tip[i];
        }
    }

    #[allow(non_snake_case)]
    let dwState = if !visible {
        flags |= NIF_STATE;
        NIS_HIDDEN
    } else {
        0
    };

    let mut nid = NOTIFYICONDATAW {
        uFlags: flags,
        hWnd: hwnd,
        uID: tray_id,
        uCallbackMessage: WM_USER_TRAYICON,
        hIcon: h_icon,
        szTip: sz_tip,
        dwState,
        dwStateMask: dwState,
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        ..std::mem::zeroed()
    };

    Shell_NotifyIconW(NIM_ADD, &mut nid as _) == TRUE
}

#[inline]
unsafe fn remove_tray_icon(hwnd: HWND, id: u32) {
    let nid = NOTIFYICONDATAW {
        uFlags: NIF_ICON,
        hWnd: hwnd,
        uID: id,
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        ..std::mem::zeroed()
    };

    if Shell_NotifyIconW(NIM_DELETE, &nid) == FALSE {
        eprintln!("Error removing system tray icon");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_anchor_rect_preserves_extreme_and_negative_coordinates() {
        for position in [
            PhysicalPosition::new(i32::MIN as f64, i32::MAX as f64),
            PhysicalPosition::new(-1250.0, -480.0),
            PhysicalPosition::new(1920.0, 1080.0),
        ] {
            let rect = cursor_anchor_rect(position);
            assert_eq!(rect.position, position);
            assert_eq!(rect.size, crate::dpi::PhysicalSize::new(1, 1));
        }
    }

    #[test]
    fn hover_state_tracks_enter_move_stationary_and_leave() {
        let first = PhysicalPosition::new(100.0, 200.0);
        let second = PhysicalPosition::new(101.0, 200.0);
        let outside = PhysicalPosition::new(140.0, 240.0);
        let mut hover = HoverState::default();

        assert_eq!(hover.update(first), HoverEvent::Enter);
        assert_eq!(hover.update(first), HoverEvent::None);
        assert_eq!(hover.leave_if_position_changed(first), None);
        assert_eq!(hover.update(second), HoverEvent::Move);
        assert_eq!(hover.leave_if_position_changed(outside), Some(second));
        assert!(!hover.entered);
        assert_eq!(hover.last_position, None);
    }

    #[test]
    fn hover_reset_makes_the_next_move_an_enter() {
        let position = PhysicalPosition::new(20.0, 30.0);
        let mut hover = HoverState::default();

        assert_eq!(hover.update(position), HoverEvent::Enter);
        hover.reset();
        assert_eq!(hover.update(position), HoverEvent::Enter);
    }

    #[test]
    fn windows_backend_has_no_blocking_rect_query_or_nested_message_pump() {
        let source = include_str!("mod.rs");
        let rect_query = ["Shell_NotifyIcon", "GetRect"].concat();
        let peek_message = ["Peek", "MessageW"].concat();
        let dispatch_message = ["Dispatch", "MessageW"].concat();

        assert!(!source.contains(&rect_query));
        assert!(!source.contains(&peek_message));
        assert!(!source.contains(&dispatch_message));
    }
}
