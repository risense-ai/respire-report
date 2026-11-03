//! 当前登录用户离席证据：复用旧通知器的锁屏优先、DWORD输入时间回绕算法。
use agentnotify_core::policy::PresenceEvidence;
/// 接收瞬间决定是否抑制；证据未知保守跳过，不能等到导入spool时重新采样。
pub fn suppress_at_receive(config: &agentnotify_core::config::ConfigV1) -> bool {
    config.presence.mode == agentnotify_core::config::PresenceMode::AwayOnly
        && NativePresence.is_away(config.presence.idle_seconds) != Some(true)
}
pub struct NativePresence;
impl PresenceEvidence for NativePresence {
    fn is_away(&self, idle_seconds: u32) -> Option<bool> {
        #[cfg(windows)] { windows_away(idle_seconds) }
        #[cfg(not(windows))] { let _=idle_seconds; None }
    }
}
#[cfg(windows)]
fn windows_away(idle_seconds: u32) -> Option<bool> {
    use windows_sys::Win32::{System::{RemoteDesktop::*, StationsAndDesktops::*, SystemInformation::GetTickCount}, UI::Input::KeyboardAndMouse::*};
    unsafe {
        // 1. WTSInfoEx为当前会话锁定状态；仅识别文档定义的0/1。
        let mut buffer=std::ptr::null_mut(); let mut size=0;
        let queried=WTSQuerySessionInformationW(std::ptr::null_mut(), WTS_CURRENT_SESSION, WTSSessionInfoEx, &mut buffer, &mut size);
        let locked=if queried!=0 && !buffer.is_null() && size as usize>=std::mem::size_of::<WTSINFOEXW>() {
            let info=&*(buffer as *const WTSINFOEXW);
            if info.Level==1 { match info.Data.WTSInfoExLevel1.SessionFlags { 0=>Some(true),1=>Some(false),_=>None } } else { None }
        } else { None };
        if !buffer.is_null() { WTSFreeMemory(buffer.cast()); }
        if locked==Some(true) { return Some(true); }
        if locked.is_none() {
            // 与旧版一致：无法访问输入桌面或切换权限被拒视为锁屏；只查询当前输入桌面。
            let desktop=OpenInputDesktop(0,false.into(),DESKTOP_SWITCHDESKTOP);
            if desktop.is_null() { return Some(true); }
            let accessible=SwitchDesktop(desktop)!=0; CloseDesktop(desktop);
            if !accessible { return Some(true); }
        }
        // 2. 最近输入时间为DWORD，wrapping_sub保证49.7天回绕与旧版一致。
        let mut info=LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime:0 };
        if GetLastInputInfo(&mut info)==0 { return None; }
        Some(GetTickCount().wrapping_sub(info.dwTime) as u64>=idle_seconds as u64*1000)
    }
}
