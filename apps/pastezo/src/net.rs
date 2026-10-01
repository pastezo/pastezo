//! One HTTP GET through the system: WinHTTP on Windows, `curl` on macOS and
//! most Linux desktops (`wget` otherwise): no HTTP or TLS code in the app.
//! Not `curl.exe` on Windows: antivirus heuristics flag a GUI app that runs it.

#[cfg(not(windows))]
use std::process::{Command, Stdio};

const USER_AGENT: &str = concat!("Pastezo/", env!("CARGO_PKG_VERSION"));
/// Longest wait for an answer, seconds.
const TIMEOUT: u32 = 15;

/// Why there is no body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// no network, or no answer in time
    Unreachable,
    /// the server answered, but not with the file: another status, or more
    /// than the limit
    Refused,
}

/// The body of a 200 answer (redirects followed), at most `limit` bytes.
pub fn get(url: &str, accept: &str, limit: usize) -> Result<Vec<u8>, Error> {
    fetch(url, accept, limit).and_then(|body| if body.len() <= limit { Ok(body) } else { Err(Error::Refused) })
}

#[cfg(not(windows))]
fn fetch(url: &str, accept: &str, limit: usize) -> Result<Vec<u8>, Error> {
    let agent = format!("User-Agent: {USER_AGENT}");
    let accept = format!("Accept: {accept}");
    let (time, size) = (TIMEOUT.to_string(), limit.to_string());
    // exit codes of an answer that is not the file: curl 22 (status 400 and up),
    // 47 (too many redirects), 63 (bigger than the limit); wget 8 (an error status)
    let curl = run(Command::new("curl").args(["-fsSL", "--max-time", &time, "--max-filesize", &size, "-H", &accept, "-H", &agent, url]), &[22, 47, 63]);
    curl.or_else(|e| match e {
        Error::Unreachable => run(Command::new("wget").args(["-qO-", &format!("--timeout={time}"), &format!("--header={accept}"), &format!("--header={agent}"), url]), &[8]),
        Error::Refused => Err(e),
    })
}

/// Its output; `refused`: the exit codes that mean the server answered.
#[cfg(not(windows))]
fn run(cmd: &mut Command, refused: &[i32]) -> Result<Vec<u8>, Error> {
    let out = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output().map_err(|_| Error::Unreachable)?;
    match out.status.code() {
        Some(0) => Ok(out.stdout),
        Some(code) if refused.contains(&code) => Err(Error::Refused),
        _ => Err(Error::Unreachable),
    }
}

/// WinHTTP: the system's proxy settings and certificates, `TIMEOUT` per step.
#[cfg(windows)]
fn fetch(url: &str, accept: &str, limit: usize) -> Result<Vec<u8>, Error> {
    use std::ffi::c_void;
    use std::ptr::null;
    use windows_sys::Win32::Networking::WinHttp::*;

    /// Closed when dropped; null: the call failed.
    struct Handle(*mut c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { WinHttpCloseHandle(self.0) };
            }
        }
    }
    let open = |h: *mut c_void| if h.is_null() { Err(Error::Unreachable) } else { Ok(Handle(h)) };
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();

    let url = url::Url::parse(url).map_err(|_| Error::Unreachable)?;
    let secure = match url.scheme() {
        "https" => WINHTTP_FLAG_SECURE,
        "http" => 0,
        _ => return Err(Error::Unreachable),
    };
    let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else { return Err(Error::Unreachable) };
    let path = &url[url::Position::BeforePath..];
    let headers = wide(&format!("Accept: {accept}"));
    let ms = (TIMEOUT * 1000) as i32;
    unsafe {
        let session = open(WinHttpOpen(wide(USER_AGENT).as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, null(), null(), 0))?;
        WinHttpSetTimeouts(session.0, ms, ms, ms, ms);
        let connection = open(WinHttpConnect(session.0, wide(host).as_ptr(), port, 0))?;
        let request = open(WinHttpOpenRequest(connection.0, wide("GET").as_ptr(), wide(path).as_ptr(), null(), null(), null(), secure))?;
        let sent = WinHttpSendRequest(request.0, headers.as_ptr(), u32::MAX, null(), 0, 0, 0);
        if sent == 0 || WinHttpReceiveResponse(request.0, std::ptr::null_mut()) == 0 {
            return Err(Error::Unreachable);
        }
        let (mut status, mut size) = (0u32, 4u32);
        let flags = WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER;
        if WinHttpQueryHeaders(request.0, flags, null(), (&raw mut status).cast(), &mut size, std::ptr::null_mut()) == 0 {
            return Err(Error::Unreachable);
        }
        if status != 200 {
            return Err(Error::Refused);
        }
        let (mut body, mut chunk) = (Vec::new(), vec![0u8; 16 * 1024]);
        loop {
            let mut read = 0u32;
            if WinHttpReadData(request.0, chunk.as_mut_ptr().cast(), chunk.len() as u32, &mut read) == 0 {
                return Err(Error::Unreachable);
            }
            if read == 0 {
                return Ok(body);
            }
            body.extend_from_slice(&chunk[..read as usize]);
            if body.len() > limit {
                return Err(Error::Refused);
            }
        }
    }
}
