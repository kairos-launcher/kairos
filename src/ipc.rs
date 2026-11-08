//! Control socket: lets `kairos --toggle` (bound to a desktop shortcut) drive the
//! resident instance. This is the hotkey path on Wayland, where apps cannot grab keys.

use std::io;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Toggle,
    Show,
    Hide,
    Quit,
}

impl Command {
    const ALL: [Self; 4] = [Self::Toggle, Self::Show, Self::Hide, Self::Quit];

    /// The word on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Toggle => "toggle",
            Self::Show => "show",
            Self::Hide => "hide",
            Self::Quit => "quit",
        }
    }
}

impl FromStr for Command {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        Self::ALL.into_iter().find(|c| c.as_str() == s).ok_or_else(|| format!("unknown command {s:?}"))
    }
}

/// Longest request line read from a client; longer ones are cut off.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
const MAX_LINE: u64 = 4096;

/// What a client asked for: a command, the xdg-activation token of the shortcut that triggered
/// it, and for `show`/`toggle` optionally text to put in the search box (`--query`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub cmd: Command,
    pub token: Option<String>,
    pub query: Option<String>,
}

impl Request {
    pub fn new(cmd: Command) -> Self {
        Self { cmd, token: None, query: None }
    }

    /// Wire format: one line, `<command>[ <token>][<TAB><query>]`. Tabs and line breaks in the
    /// query become spaces.
    #[cfg_attr(not(any(unix, windows)), allow(dead_code))]
    fn to_line(&self) -> String {
        let mut line = self.cmd.as_str().to_string();
        if let Some(t) = &self.token {
            line += " ";
            line += t;
        }
        if let Some(q) = &self.query {
            line.push('\t');
            line.extend(q.chars().map(|c| if matches!(c, '\t' | '\r' | '\n') { ' ' } else { c }));
        }
        line.push('\n');
        line
    }
}

/// Parse one request line (see `Request::to_line`).
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
pub fn parse_line(line: &str) -> Result<Request, String> {
    let line = line.trim_end_matches(['\r', '\n']);
    let (head, query) = match line.split_once('\t') {
        Some((h, q)) => (h, Some(q.to_string())),
        None => (line, None),
    };
    let mut it = head.split_whitespace();
    let cmd = it.next().unwrap_or("").parse()?;
    Ok(Request { cmd, token: it.next().map(str::to_string), query })
}

/// The activation token the desktop gave this process, if it is safe to send.
#[cfg_attr(not(unix), allow(dead_code))]
fn own_activation_token() -> Option<String> {
    std::env::var("XDG_ACTIVATION_TOKEN").ok().filter(|t| !t.is_empty() && !t.contains(char::is_whitespace))
}

#[cfg(not(any(unix, windows)))]
pub use other::*;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
pub use windows::*;

#[cfg(unix)]
mod unix {
    use super::{MAX_LINE, Request, io, parse_line};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};

    /// `$XDG_RUNTIME_DIR/kairos.sock` (private to the user by spec), else a socket in a
    /// private folder of the user's cache directory. Never a shared folder such as `/tmp`,
    /// where another local user could claim the name first.
    pub fn socket_path() -> io::Result<PathBuf> {
        if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
            return Ok(PathBuf::from(d).join("kairos.sock"));
        }
        let dir = dirs::cache_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no runtime or cache directory"))?
            .join("kairos");
        make_private_dir(&dir)?;
        Ok(dir.join("kairos.sock"))
    }

    /// Create `dir` (mode 0700) or tighten an existing one. Changing the mode fails unless we
    /// own the folder, so a folder planted by someone else is refused.
    fn make_private_dir(dir: &Path) -> io::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        let meta = std::fs::symlink_metadata(dir)?;
        if !meta.is_dir() {
            return Err(io::Error::other(format!("{} is not a folder", dir.display())));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    /// Send a command to the running instance, forwarding the activation token the desktop
    /// gave this process so the compositor lets the launcher window take focus.
    pub fn send(req: &Request) -> io::Result<()> {
        let mut s = UnixStream::connect(socket_path()?)?;
        let req = Request { token: super::own_activation_token(), ..req.clone() };
        s.write_all(req.to_line().as_bytes())
    }

    /// Owns the socket file; removes it on drop.
    pub struct Server {
        listener: UnixListener,
        path: PathBuf,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// Claim the socket. `Ok(None)` means another instance is already serving it.
    pub fn bind() -> io::Result<Option<Server>> {
        bind_at(socket_path()?)
    }

    pub(super) fn bind_at(path: PathBuf) -> io::Result<Option<Server>> {
        if UnixStream::connect(&path).is_ok() {
            return Ok(None);
        }
        // Nobody answered: a leftover file from a crashed instance.
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        // The folder is private already; this keeps the socket private if it is ever moved.
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Some(Server { listener, path }))
    }

    impl Server {
        /// Blocking accept loop on its own thread; no polling. The caller keeps `self`
        /// alive for the process lifetime so the socket file is removed on exit.
        pub fn serve(&self, on_request: impl Fn(Request) + Send + 'static) -> io::Result<()> {
            let listener = self.listener.try_clone()?;
            std::thread::spawn(move || {
                for conn in listener.incoming().flatten() {
                    // A client that connects and sends nothing must not block the next one.
                    let _ = conn.set_read_timeout(Some(std::time::Duration::from_secs(1)));
                    let mut line = String::new();
                    // An empty line is just the single-instance probe connecting.
                    if BufReader::new(conn).take(MAX_LINE).read_line(&mut line).is_ok_and(|n| n > 0) {
                        match parse_line(&line) {
                            Ok(req) => on_request(req),
                            Err(e) => log::warn!("ipc: {e}"),
                        }
                    }
                }
            });
            Ok(())
        }
    }

    #[cfg(test)]
    pub(super) mod tests {
        use super::*;
        use std::sync::mpsc::channel;
        use std::time::Duration;

        #[test]
        fn private_dir_is_created_and_tightened() {
            use std::os::unix::fs::PermissionsExt;
            let tmp_dir = crate::test_support::tmp();
            let dir = tmp_dir.path().to_path_buf();
            make_private_dir(&dir).unwrap();
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            make_private_dir(&dir).unwrap();
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
            let _ = std::fs::remove_dir(dir);
        }

        #[test]
        fn roundtrip_single_instance_and_stale_socket() {
            let tmp_path = crate::test_support::tmp();
            let path = tmp_path.path().join("path.sock");
            let server = bind_at(path.clone()).unwrap().expect("first bind wins");
            assert!(bind_at(path.clone()).unwrap().is_none(), "second instance must see the first");

            let (tx, rx) = channel();
            server.serve(move |r| tx.send(r).unwrap()).unwrap();
            // A client that never writes must not block the ones after it.
            let _silent = UnixStream::connect(&path).unwrap();
            let mut s = UnixStream::connect(&path).unwrap();
            writeln!(s, "toggle tok123\tf report").unwrap();
            let got = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(got.cmd, super::super::Command::Toggle);
            assert_eq!((got.token.as_deref(), got.query.as_deref()), (Some("tok123"), Some("f report")));

            // A leftover file with no listener is reclaimed.
            let tmp_stale = crate::test_support::tmp();
            let stale = tmp_stale.path().join("stale.sock");
            drop(UnixListener::bind(&stale).unwrap()); // file stays, nobody listening
            assert!(bind_at(stale).unwrap().is_some());
        }
    }
}

/// Named pipe `\\.\pipe\kairos-<user>`. Owning its first instance makes us the single instance;
/// Windows removes the pipe when the process exits, so nothing goes stale.
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows {
    use super::{MAX_LINE, Request, io, parse_line};
    use std::fs::File;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::sync::Mutex;
    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeServerProcessId, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, WaitNamedPipeW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;

    /// One name per user, so users on the same machine do not collide.
    pub fn pipe_name() -> String {
        let user = std::env::var("USERNAME").unwrap_or_default();
        let user: String =
            user.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        format!(r"\\.\pipe\kairos-{user}")
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Send a command to the running instance.
    pub fn send(req: &Request) -> io::Result<()> {
        send_to(&pipe_name(), req)
    }

    /// Open a client end, waiting briefly if every instance is busy.
    fn connect(name: &str) -> io::Result<File> {
        let open = || std::fs::OpenOptions::new().write(true).open(name);
        match open() {
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                // SAFETY: `wide` returns a NUL-terminated UTF-16 string that outlives the call.
                unsafe { WaitNamedPipeW(wide(name).as_ptr(), 1000) };
                open()
            }
            r => r,
        }
    }

    pub(super) fn send_to(name: &str, req: &Request) -> io::Result<()> {
        let mut f = connect(name)?;
        // We were just started by the user, so we may hand our right to take the foreground to
        // the launcher; then its window gets focus without tricks.
        let mut pid = 0u32;
        // SAFETY: `f` is an open pipe client handle; `pid` is a valid out pointer.
        if unsafe { GetNamedPipeServerProcessId(f.as_raw_handle(), &raw mut pid) } != 0 {
            // SAFETY: plain call; failure only means focus falls back to the window's own request.
            unsafe { AllowSetForegroundWindow(pid) };
        }
        f.write_all(req.to_line().as_bytes())
    }

    pub struct Server {
        /// The listening instance from `bind`, taken by `serve`.
        first: Mutex<Option<OwnedHandle>>,
        name: Vec<u16>,
    }

    /// Claim the pipe. `Ok(None)` means another instance is already serving it.
    pub fn bind() -> io::Result<Option<Server>> {
        bind_at(&pipe_name())
    }

    pub(super) fn bind_at(name: &str) -> io::Result<Option<Server>> {
        // Someone answering on the name means another instance runs. Real Windows also refuses a
        // second first-instance below; Wine does not, so check here as well.
        match std::fs::OpenOptions::new().write(true).open(name) {
            Ok(_) => return Ok(None),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => return Ok(None),
            Err(_) => {}
        }
        let name = wide(name);
        match create_instance(&name, true) {
            Ok(h) => Ok(Some(Server { first: Mutex::new(Some(h)), name })),
            Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The default security lets only the owner (and SYSTEM) write, so other users cannot send
    /// commands.
    fn create_instance(name: &[u16], first: bool) -> io::Result<OwnedHandle> {
        let open_mode = PIPE_ACCESS_INBOUND | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
        let pipe_mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
        // SAFETY: `name` is NUL-terminated UTF-16; a null security pointer selects the default.
        let h = unsafe {
            CreateNamedPipeW(name.as_ptr(), open_mode, pipe_mode, PIPE_UNLIMITED_INSTANCES, 0, 512, 0, std::ptr::null())
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `h` is a valid handle we own from here on.
        Ok(unsafe { OwnedHandle::from_raw_handle(h) })
    }

    impl Server {
        /// Blocking accept loop on its own thread; no polling. Each client is read on a thread
        /// of its own, so one that connects and sends nothing blocks nobody.
        pub fn serve(&self, on_request: impl Fn(Request) + Send + 'static) -> io::Result<()> {
            let Some(mut pipe) = self.first.lock().map_err(|_| io::Error::other("ipc lock poisoned"))?.take() else {
                return Err(io::Error::other("already serving"));
            };
            let name = self.name.clone();
            // Readers hand requests to one thread that runs `on_request`.
            let (tx, rx) = std::sync::mpsc::channel::<Request>();
            std::thread::spawn(move || {
                for req in rx {
                    on_request(req);
                }
            });
            std::thread::spawn(move || {
                loop {
                    // SAFETY: `pipe` is a valid pipe server handle; no overlapped I/O.
                    let ok = unsafe { ConnectNamedPipe(pipe.as_raw_handle(), std::ptr::null_mut()) } != 0;
                    // A client that came before we listened counts as connected (PIPE_CONNECTED),
                    // also if it is already gone again (NO_DATA, e.g. the single-instance probe).
                    let err = if ok { None } else { io::Error::last_os_error().raw_os_error() };
                    let connected =
                        ok || matches!(err, Some(e) if e == ERROR_PIPE_CONNECTED as i32 || e == ERROR_NO_DATA as i32);
                    // Have the next instance waiting before handling this client. Also replaces
                    // an instance that failed, so the loop never spins on it.
                    let next = match create_instance(&name, false) {
                        Ok(h) => h,
                        Err(e) => {
                            log::error!("ipc: cannot create pipe instance: {e}");
                            return;
                        }
                    };
                    let done = std::mem::replace(&mut pipe, next);
                    if !connected {
                        log::warn!("ipc: {}", io::Error::from_raw_os_error(err.unwrap_or_default()));
                        continue;
                    }
                    let conn = File::from(done);
                    let tx = tx.clone();
                    std::thread::spawn(move || {
                        let mut line = String::new();
                        if BufReader::new(conn).take(MAX_LINE).read_line(&mut line).is_ok_and(|n| n > 0) {
                            match parse_line(&line) {
                                Ok(msg) => {
                                    let _ = tx.send(msg);
                                }
                                Err(e) => log::warn!("ipc: {e}"),
                            }
                        }
                    });
                }
            });
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::mpsc::channel;
        use std::time::Duration;

        #[test]
        fn roundtrip_single_instance() {
            let name = format!(r"\\.\pipe\kairos-test-{}", std::process::id());
            let server = bind_at(&name).unwrap().expect("first bind wins");
            assert!(bind_at(&name).unwrap().is_none(), "second instance must see the first");

            let (tx, rx) = channel();
            server.serve(move |r| tx.send(r).unwrap()).unwrap();
            // A client that never writes must not block the ones after it.
            let _silent = connect(&name).unwrap();
            let req = Request { query: Some("f a\tb".into()), ..Request::new(super::super::Command::Toggle) };
            send_to(&name, &req).unwrap();
            let got = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(got.query.as_deref(), Some("f a b"));
        }
    }
}

/// Stand-in with the same API so callers need no `cfg`.
#[cfg(not(any(unix, windows)))]
#[allow(clippy::unnecessary_wraps, clippy::unused_self)]
mod other {
    use super::{Request, io};

    pub struct Server;

    pub fn send(_req: &Request) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "control socket is not implemented on this OS"))
    }

    pub fn bind() -> io::Result<Option<Server>> {
        Ok(Some(Server))
    }

    impl Server {
        pub fn serve(&self, _on_request: impl Fn(Request) + Send + 'static) -> io::Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert_eq!("toggle\n".parse::<Command>(), Ok(Command::Toggle));
        assert_eq!(" quit ".parse::<Command>(), Ok(Command::Quit));
        assert!("bogus".parse::<Command>().is_err());
        for c in Command::ALL {
            assert_eq!(c.as_str().parse::<Command>(), Ok(c));
        }
        let r = |cmd, token: Option<&str>, query: Option<&str>| Request {
            cmd,
            token: token.map(str::to_string),
            query: query.map(str::to_string),
        };
        assert_eq!(parse_line("show abc\n"), Ok(r(Command::Show, Some("abc"), None)));
        assert_eq!(parse_line("hide"), Ok(r(Command::Hide, None, None)));
        assert_eq!(parse_line("show\t= 2 + 2 \n"), Ok(r(Command::Show, None, Some("= 2 + 2 "))));
        assert_eq!(parse_line("show tok\t"), Ok(r(Command::Show, Some("tok"), Some(""))));
        assert!(parse_line("").is_err());
        // Round trip, with characters that must not break the line.
        let req = r(Command::Show, Some("t"), Some("a\tb\nc"));
        assert_eq!(parse_line(&req.to_line()), Ok(r(Command::Show, Some("t"), Some("a b c"))));
    }
}
