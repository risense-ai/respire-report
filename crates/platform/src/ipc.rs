//! 仅本机当前用户命名管道；固定协议、有界报文和超时，失败不声称事件已持久化。
use crate::{PlatformError, Result, paths::PlatformPaths};
use agentnotify_core::{
    contracts::{EnqueueOutcome, HealthReport},
    event::AdapterOutput,
};
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Health,
    Stop,
    Pause {
        paused: bool,
    },
    Ingest {
        id: uuid::Uuid,
        output: AdapterOutput,
        /// CLI本机采样结果，仅授予抑制能力；外部事件JSON不提供此字段。
        presence_suppressed: bool,
        /// 本机准入签名，认证事件、接收 ID 和离席决定。旧请求缺字段时明确拒绝。
        #[serde(default)]
        admission_tag: Option<[u8; 32]>,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Health { report: HealthReport },
    Accepted { outcome: EnqueueOutcome },
    Ok,
    Error { code: String },
}
fn pipe_name(paths: &PlatformPaths) -> Result<String> {
    use sha2::{Digest, Sha256};
    #[cfg(windows)]
    let identity = crate::windows::current_sid()?;
    #[cfg(not(windows))]
    let identity = String::new();
    // 调用方已验证安全目录；用实际目录的规范路径统一斜杠和文件系统大小写表示。
    // 仅改变管道身份计算，不放宽当前用户SID、管道DACL或目录归属检查。
    #[cfg(windows)]
    let root = std::fs::canonicalize(&paths.data).map_err(|_| PlatformError("ipc_root_unavailable"))?;
    #[cfg(not(windows))]
    let root = paths.data.clone();
    let hash = Sha256::digest(format!("{}|{}", identity, root.display()).as_bytes());
    Ok(format!(
        r"\\.\pipe\AgentNotify.{}",
        hash.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}
pub fn call(paths: &PlatformPaths, request: &Request) -> Result<Response> {
    #[cfg(windows)]
    {
        win::call(&pipe_name(paths)?, request)
    }
    #[cfg(not(windows))]
    {
        let _ = (paths, request);
        Err(PlatformError("native_ipc_pending"))
    }
}
pub struct Server {
    #[cfg(windows)]
    inner: win::Server,
}
impl Server {
    pub fn bind(paths: &PlatformPaths) -> Result<Self> {
        #[cfg(windows)]
        {
            Ok(Self {
                inner: win::Server::bind(&pipe_name(paths)?)?,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = paths;
            Err(PlatformError("native_ipc_pending"))
        }
    }
    /// 优雅停止时先等最后ACK；不因立刻关闭管道丢弃停止响应。
    pub fn finish_response(&mut self) {
        #[cfg(windows)]
        {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(1);
            while self.inner.responded && std::time::Instant::now() < end {
                let _ = self.inner.poll(|_| Response::Error {
                    code: "shutting_down".into(),
                });
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
    pub fn poll(&mut self, handle: impl FnOnce(Request) -> Response) -> Result<()> {
        #[cfg(windows)]
        {
            self.inner.poll(handle)
        }
        #[cfg(not(windows))]
        {
            let _ = handle;
            Err(PlatformError("native_ipc_pending"))
        }
    }
}
#[cfg(windows)]
mod win {
    use super::*;
    use crate::windows::{Handle, wide};
    use std::{
        ptr::{null, null_mut},
        time::{Duration, Instant},
    };
    use windows_sys::Win32::{
        Foundation::*, Security::*, Storage::FileSystem::*, System::Pipes::*,
    };
    const MAX: u32 = 131072;
    fn read(h: HANDLE) -> Result<Option<Vec<u8>>> {
        unsafe {
            let mut b = vec![0; MAX as usize];
            let mut n = 0;
            if ReadFile(h, b.as_mut_ptr().cast(), MAX, &mut n, null_mut()) == 0 {
                let e = GetLastError();
                if e == ERROR_NO_DATA || e == ERROR_PIPE_LISTENING {
                    return Ok(None);
                }
                return Err(PlatformError("ipc_read_failed"));
            }
            b.truncate(n as usize);
            Ok(Some(b))
        }
    }
    fn write(h: HANDLE, b: &[u8]) -> Result<()> {
        unsafe {
            if b.len() > MAX as usize {
                return Err(PlatformError("ipc_message_too_large"));
            }
            let mut n = 0;
            if WriteFile(h, b.as_ptr().cast(), b.len() as u32, &mut n, null_mut()) == 0
                || n != b.len() as u32
            {
                return Err(PlatformError("ipc_write_failed"));
            }
            Ok(())
        }
    }
    pub fn call(name: &str, request: &Request) -> Result<Response> {
        unsafe {
            let name = wide(name);
            if WaitNamedPipeW(name.as_ptr(), 150) == 0 {
                return Err(PlatformError("daemon_unavailable"));
            }
            let h = CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            );
            if h == INVALID_HANDLE_VALUE {
                return Err(PlatformError("daemon_unavailable"));
            }
            let h = Handle(h);
            let mode = PIPE_READMODE_MESSAGE | PIPE_NOWAIT;
            if SetNamedPipeHandleState(h.0, &mode, null(), null()) == 0 {
                return Err(PlatformError("ipc_mode_failed"));
            }
            let raw = serde_json::to_vec(request).map_err(|_| PlatformError("ipc_invalid"))?;
            write(h.0, &raw)?;
            let end = Instant::now() + Duration::from_millis(700);
            loop {
                if let Some(raw) = read(h.0)? {
                    let response =
                        serde_json::from_slice(&raw).map_err(|_| PlatformError("ipc_invalid"))?;
                    let _ = write(h.0, b"ack");
                    return Ok(response);
                }
                if Instant::now() >= end {
                    return Err(PlatformError("ipc_timeout"));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    pub struct Server {
        handle: Handle,
        connected: Option<Instant>,
        pub(super) responded: bool,
    }
    impl Server {
        pub fn bind(name: &str) -> Result<Self> {
            unsafe {
                let sd = crate::windows::descriptor(&crate::windows::current_sid()?)?;
                let sa = SECURITY_ATTRIBUTES {
                    nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: sd.0,
                    bInheritHandle: 0,
                };
                let h = CreateNamedPipeW(
                    wide(name).as_ptr(),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                    PIPE_TYPE_MESSAGE
                        | PIPE_READMODE_MESSAGE
                        | PIPE_NOWAIT
                        | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    MAX,
                    MAX,
                    0,
                    &sa,
                );
                if h == INVALID_HANDLE_VALUE {
                    return Err(PlatformError("ipc_bind_failed"));
                }
                Ok(Self {
                    handle: Handle(h),
                    connected: None,
                    responded: false,
                })
            }
        }
        pub fn poll(&mut self, handle: impl FnOnce(Request) -> Response) -> Result<()> {
            unsafe {
                if self.connected.is_none() {
                    // 非阻塞管道首次进入监听时也可能返回成功，须等到客户端实际连接。
                    if ConnectNamedPipe(self.handle.0, null_mut()) != 0 {
                        return Ok(());
                    }
                    match GetLastError() {
                        ERROR_PIPE_CONNECTED => self.connected = Some(Instant::now()),
                        ERROR_PIPE_LISTENING => return Ok(()),
                        ERROR_NO_DATA => {
                            // 客户端在轮询前关闭。断开旧连接，下次轮询重新监听。
                            if DisconnectNamedPipe(self.handle.0) == 0 {
                                return Err(PlatformError("ipc_disconnect_failed"));
                            }
                            return Ok(());
                        }
                        _ => return Err(PlatformError("ipc_connect_failed")),
                    }
                }
                if self.responded {
                    if matches!(read(self.handle.0), Ok(None))
                        && self.connected.unwrap().elapsed() < Duration::from_secs(1)
                    {
                        return Ok(());
                    }
                    DisconnectNamedPipe(self.handle.0);
                    self.connected = None;
                    self.responded = false;
                    return Ok(());
                }
                match read(self.handle.0) {
                    Ok(Some(raw)) => {
                        let response = match serde_json::from_slice(&raw) {
                            Ok(r) => handle(r),
                            Err(_) => Response::Error {
                                code: "invalid_request".into(),
                            },
                        };
                        let result = write(
                            self.handle.0,
                            &serde_json::to_vec(&response)
                                .map_err(|_| PlatformError("ipc_invalid"))?,
                        );
                        // 客户端先读ACK再关闭；不Flush（可能阻塞），保留连接到下一轮/客户端断开。
                        self.connected = Some(Instant::now());
                        self.responded = true;
                        result
                    }
                    Ok(None) if self.connected.unwrap().elapsed() < Duration::from_secs(1) => {
                        Ok(())
                    }
                    _ => {
                        DisconnectNamedPipe(self.handle.0);
                        self.connected = None;
                        Ok(())
                    }
                }
            }
        }
    }

    #[cfg(test)]
    #[test]
    fn pipe_accepts_new_peer_after_previous_peer_leaves_before_poll() {
        let name = format!(r"\\.\pipe\AgentNotify-test-{}", uuid::Uuid::new_v4());
        let mut server = Server::bind(&name).unwrap();
        let name = wide(&name);
        let unexpected = |_| panic!("abandoned peer cannot submit a request");
        server.poll(unexpected).unwrap();

        // 客户端在服务端下一次轮询前关闭，Windows 要求显式断开旧连接。
        unsafe {
            let peer = CreateFileW(
                name.as_ptr(), GENERIC_READ | GENERIC_WRITE, 0, null(), OPEN_EXISTING, 0, null_mut(),
            );
            assert_ne!(peer, INVALID_HANDLE_VALUE);
            drop(Handle(peer));
        }
        server.poll(unexpected).unwrap();
        server.poll(unexpected).unwrap();

        assert_ne!(unsafe { WaitNamedPipeW(name.as_ptr(), 20) }, 0);
    }
}
