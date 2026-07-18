//! Platform-native, same-user transport for daemon IPC.
//!
//! Unix keeps the owner-only Unix socket. Windows uses a per-user named pipe
//! whose name and security descriptor are both derived from the current user's
//! SID. Protocol framing remains independent of the concrete stream types.

use anyhow::{Context, Result};
use std::fmt;

use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

#[cfg(windows)]
use std::ffi::{c_void, OsStr, OsString};
#[cfg(windows)]
use std::io::Write;
#[cfg(windows)]
use std::mem::{size_of, size_of_val};
#[cfg(windows)]
use std::os::windows::ffi::{OsStrExt, OsStringExt};
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle};
#[cfg(windows)]
use std::ptr::null_mut;
#[cfg(windows)]
use std::time::Duration;
#[cfg(windows)]
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_ALREADY_EXISTS, ERROR_INSUFFICIENT_BUFFER, ERROR_PIPE_BUSY,
    ERROR_SUCCESS, GENERIC_WRITE, HANDLE, HLOCAL, INVALID_HANDLE_VALUE,
};
#[cfg(windows)]
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_KERNEL_OBJECT,
};
#[cfg(windows)]
use windows_sys::Win32::Security::{
    CopySid, EqualSid, GetLengthSid, GetTokenInformation, IsValidSid, SetFileSecurityW,
    SetKernelObjectSecurity, TokenUser, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
    TOKEN_QUERY, TOKEN_USER,
};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    CreateDirectoryW, CreateFileW, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, WRITE_DAC, WRITE_OWNER,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// The daemon endpoint on the current platform.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Endpoint {
    #[cfg(unix)]
    path: PathBuf,
    #[cfg(windows)]
    pipe_name: String,
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        #[cfg(unix)]
        return self.path.display().fmt(f);
        #[cfg(windows)]
        return self.pipe_name.fmt(f);
    }
}

/// Resolve the daemon endpoint for the current user.
pub fn endpoint() -> Result<Endpoint> {
    #[cfg(unix)]
    {
        Ok(Endpoint {
            path: crate::config::Config::socket_path()?,
        })
    }

    #[cfg(windows)]
    {
        let user = CurrentUser::load().context("Failed to determine current Windows user SID")?;
        Ok(Endpoint {
            pipe_name: format!(r"\\.\pipe\incant-{}", user.sid_string),
        })
    }
}

#[cfg(unix)]
pub type ClientStream = UnixStream;
#[cfg(windows)]
pub type ClientStream = NamedPipeClient;

#[cfg(unix)]
pub type ServerStream = UnixStream;
#[cfg(windows)]
pub type ServerStream = NamedPipeServer;

/// A platform-native daemon listener.
pub struct Listener {
    #[cfg(unix)]
    inner: UnixListener,
    #[cfg(windows)]
    endpoint: Endpoint,
    #[cfg(windows)]
    user: CurrentUser,
    #[cfg(windows)]
    next: NamedPipeServer,
}

impl Listener {
    /// Bind the endpoint with same-user-only access.
    pub fn bind(endpoint: &Endpoint) -> Result<Self> {
        #[cfg(unix)]
        {
            if let Some(parent) = endpoint.path.parent() {
                ensure_private_dir(parent).with_context(|| {
                    format!("Failed to secure socket directory: {}", parent.display())
                })?;
            }
            if endpoint.path.exists() {
                std::fs::remove_file(&endpoint.path).with_context(|| {
                    format!(
                        "Failed to remove existing socket: {}",
                        endpoint.path.display()
                    )
                })?;
            }
            let inner = UnixListener::bind(&endpoint.path).with_context(|| {
                format!("Failed to bind to socket: {}", endpoint.path.display())
            })?;
            restrict_to_owner(&endpoint.path).with_context(|| {
                format!(
                    "Failed to restrict socket permissions: {}",
                    endpoint.path.display()
                )
            })?;
            Ok(Self { inner })
        }

        #[cfg(windows)]
        {
            let user =
                CurrentUser::load().context("Failed to determine current Windows user SID")?;
            let expected_name = format!(r"\\.\pipe\incant-{}", user.sid_string);
            if endpoint.pipe_name != expected_name {
                anyhow::bail!("Named-pipe endpoint does not match the current user SID");
            }
            let next = create_server_instance(endpoint, &user, true)
                .with_context(|| format!("Failed to create named pipe: {endpoint}"))?;
            Ok(Self {
                endpoint: endpoint.clone(),
                user,
                next,
            })
        }
    }

    /// Accept one client. Windows creates the replacement pipe instance before
    /// returning the connected one so simultaneous clients never see a gap.
    pub async fn accept(&mut self) -> Result<ServerStream> {
        #[cfg(unix)]
        {
            let (stream, _) = self.inner.accept().await?;
            Ok(stream)
        }

        #[cfg(windows)]
        {
            self.next.connect().await?;
            let replacement = create_server_instance(&self.endpoint, &self.user, false)
                .with_context(|| {
                    format!(
                        "Failed to create next named-pipe instance: {}",
                        self.endpoint
                    )
                })?;
            Ok(std::mem::replace(&mut self.next, replacement))
        }
    }
}

/// Connect to the daemon, enforcing platform identity checks before returning.
pub async fn connect(endpoint: &Endpoint) -> Result<ClientStream> {
    #[cfg(unix)]
    {
        Ok(UnixStream::connect(&endpoint.path).await?)
    }

    #[cfg(windows)]
    {
        const MAX_BUSY_RETRIES: usize = 20;
        const BUSY_RETRY_DELAY: Duration = Duration::from_millis(50);

        let user = CurrentUser::load().context("Failed to determine current Windows user SID")?;
        let mut retries = 0;
        let stream = loop {
            match ClientOptions::new().open(&endpoint.pipe_name) {
                Ok(stream) => break stream,
                Err(error)
                    if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
                        && retries < MAX_BUSY_RETRIES =>
                {
                    retries += 1;
                    tokio::time::sleep(BUSY_RETRY_DELAY).await;
                }
                Err(error) => return Err(error.into()),
            }
        };

        verify_pipe_owner(&stream, &user.sid)
            .context("Named-pipe server owner is not the current user")?;
        Ok(stream)
    }
}

/// Remove persistent endpoint state, if the platform has any.
pub async fn cleanup(endpoint: &Endpoint) -> Result<()> {
    #[cfg(unix)]
    {
        if endpoint.path.exists() {
            tokio::fs::remove_file(&endpoint.path).await?;
        }
    }
    #[cfg(windows)]
    {
        // Named pipes are kernel objects and disappear with their last handle.
        let _ = endpoint;
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(dir)?;
    let mode = std::fs::metadata(dir)?.permissions().mode();
    if mode & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        tracing::info!("Tightened permissions on {} to 0700", dir.display());
    }
    Ok(())
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "private file has no parent directory",
        )
    })?;
    ensure_private_dir(parent)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(contents)?;
    file.flush()
}

/// Create or tighten a Windows directory so only the current user can access
/// it. Missing ancestors below the first existing parent are created with the
/// secure descriptor already attached.
#[cfg(windows)]
pub(crate) fn ensure_private_directory(path: &Path) -> std::io::Result<()> {
    let user = CurrentUser::load()?;
    let mut missing = Vec::new();
    let mut cursor = path;
    while !cursor.exists() {
        missing.push(cursor.to_path_buf());
        cursor = cursor.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "private directory has no existing ancestor",
            )
        })?;
    }

    for directory in missing.iter().rev() {
        create_private_directory(directory, &user)?;
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "private directory path is not a regular directory: {}",
                path.display()
            ),
        ));
    }
    secure_path(path, SecurityDescriptor::for_directory(&user.sid_string)?)
}

/// Apply the current-user-only descriptor to an existing Windows file before
/// reading potentially sensitive configuration from it.
#[cfg(windows)]
pub(crate) fn secure_existing_file(path: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "private file path is not a regular file: {}",
                path.display()
            ),
        ));
    }
    let user = CurrentUser::load()?;
    secure_path(path, SecurityDescriptor::for_file(&user.sid_string)?)
}

/// Atomically create/truncate and write a file through a handle whose security
/// descriptor grants access only to the current user. Existing files are
/// re-secured through the handle before any new contents are written.
#[cfg(windows)]
pub(crate) fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "private file has no parent directory",
        )
    })?;
    ensure_private_directory(parent)?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "private file path is not a regular file: {}",
                    path.display()
                ),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let user = CurrentUser::load()?;
    let mut descriptor = SecurityDescriptor::for_file(&user.sid_string)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.as_mut_ptr(),
        bInheritHandle: 0,
    };
    let wide = wide_path(path)?;
    // SAFETY: the path is NUL terminated, the security descriptor outlives the
    // call, and the returned handle is immediately placed under RAII.
    let raw_handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE | WRITE_DAC | WRITE_OWNER,
            0,
            &attributes,
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    let handle = OwnedHandle::new(raw_handle)?;
    secure_handle(handle.0, &mut descriptor)?;
    // SAFETY: ownership is transferred exactly once from OwnedHandle to File.
    let mut file = unsafe { std::fs::File::from_raw_handle(handle.into_inner() as _) };
    file.write_all(contents)?;
    file.flush()
}

#[cfg(windows)]
fn create_private_directory(path: &Path, user: &CurrentUser) -> std::io::Result<()> {
    let mut descriptor = SecurityDescriptor::for_directory(&user.sid_string)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.as_mut_ptr(),
        bInheritHandle: 0,
    };
    let wide = wide_path(path)?;
    // SAFETY: the NUL-terminated path and security attributes remain valid for
    // the duration of CreateDirectoryW.
    if unsafe { CreateDirectoryW(wide.as_ptr(), &attributes) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
            return Err(error);
        }
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "private directory path is not a regular directory: {}",
                path.display()
            ),
        ));
    }
    secure_path(path, descriptor)
}

#[cfg(windows)]
fn secure_path(path: &Path, mut descriptor: SecurityDescriptor) -> std::io::Result<()> {
    let wide = wide_path(path)?;
    // SAFETY: the path and self-relative descriptor are valid for this call.
    if unsafe {
        SetFileSecurityW(
            wide.as_ptr(),
            private_security_information(),
            descriptor.as_mut_ptr(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn secure_handle(handle: HANDLE, descriptor: &mut SecurityDescriptor) -> std::io::Result<()> {
    // SAFETY: `handle` is live and was opened with WRITE_DAC | WRITE_OWNER.
    if unsafe {
        SetKernelObjectSecurity(
            handle,
            private_security_information(),
            descriptor.as_mut_ptr(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn private_security_information() -> u32 {
    OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION
}

#[cfg(windows)]
fn wide_path(path: &Path) -> std::io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Windows path contains an interior NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}
#[cfg(windows)]
fn create_server_instance(
    endpoint: &Endpoint,
    user: &CurrentUser,
    first: bool,
) -> std::io::Result<NamedPipeServer> {
    let mut descriptor = SecurityDescriptor::for_pipe(&user.sid_string)?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.as_mut_ptr(),
        bInheritHandle: 0,
    };
    let mut options = ServerOptions::new();
    options
        .reject_remote_clients(true)
        .first_pipe_instance(first);

    // SAFETY: `attributes` and its descriptor remain valid for the duration of
    // CreateNamedPipeW. The kernel captures the descriptor before this call
    // returns, so both RAII allocations can then be released.
    unsafe {
        options.create_with_security_attributes_raw(
            &endpoint.pipe_name,
            &mut attributes as *mut SECURITY_ATTRIBUTES as *mut c_void,
        )
    }
}

#[cfg(windows)]
fn verify_pipe_owner(stream: &NamedPipeClient, expected: &OwnedSid) -> std::io::Result<()> {
    let mut owner: PSID = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();

    // SAFETY: the pipe handle is live for this call. GetSecurityInfo initializes
    // `owner` to point inside its LocalAlloc-backed descriptor on success.
    let status = unsafe {
        GetSecurityInfo(
            stream.as_raw_handle() as HANDLE,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(status as i32));
    }
    let descriptor = LocalAllocation::new(descriptor as HLOCAL)?;
    if owner.is_null() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "named pipe has no owner SID",
        ));
    }

    // SAFETY: both SIDs are valid while `descriptor` and `expected` are alive.
    let matches = unsafe { IsValidSid(owner) != 0 && EqualSid(owner, expected.as_psid()) != 0 };
    drop(descriptor);
    if !matches {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "named-pipe owner SID does not match current user SID",
        ));
    }
    Ok(())
}

#[cfg(windows)]
struct CurrentUser {
    sid: OwnedSid,
    sid_string: String,
}

#[cfg(windows)]
impl CurrentUser {
    fn load() -> std::io::Result<Self> {
        let mut token: HANDLE = null_mut();
        // SAFETY: `token` is a valid out pointer; the pseudo-process handle is
        // always valid for the current process.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let token = OwnedHandle::new(token)?;

        let mut byte_len = 0;
        // SAFETY: the documented sizing call uses a null buffer and zero size.
        let sized =
            unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut byte_len) };
        if sized != 0
            || std::io::Error::last_os_error().raw_os_error()
                != Some(ERROR_INSUFFICIENT_BUFFER as i32)
        {
            return Err(std::io::Error::last_os_error());
        }
        if byte_len < size_of::<TOKEN_USER>() as u32 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "TokenUser buffer is unexpectedly small",
            ));
        }

        // `usize` storage provides sufficient alignment for TOKEN_USER and SID.
        let words = (byte_len as usize).div_ceil(size_of::<usize>());
        let mut token_info = vec![0usize; words];
        // SAFETY: the aligned allocation is at least `byte_len` bytes long.
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                token_info.as_mut_ptr() as *mut c_void,
                byte_len,
                &mut byte_len,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: GetTokenInformation initialized a TOKEN_USER at the buffer.
        let token_user = unsafe { &*(token_info.as_ptr() as *const TOKEN_USER) };
        let sid = OwnedSid::copy_from(token_user.User.Sid)?;
        let sid_string = sid.to_string()?;
        Ok(Self { sid, sid_string })
    }
}

#[cfg(windows)]
struct OwnedSid {
    storage: Vec<usize>,
    byte_len: u32,
}

#[cfg(windows)]
impl OwnedSid {
    fn copy_from(source: PSID) -> std::io::Result<Self> {
        if source.is_null() || unsafe { IsValidSid(source) } == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Windows token contains an invalid user SID",
            ));
        }
        // SAFETY: `source` was validated above.
        let byte_len = unsafe { GetLengthSid(source) };
        if byte_len == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let words = (byte_len as usize).div_ceil(size_of::<usize>());
        let mut storage = vec![0usize; words];
        // SAFETY: destination storage is aligned and at least `byte_len` bytes.
        if unsafe { CopySid(byte_len, storage.as_mut_ptr() as PSID, source) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { storage, byte_len })
    }

    fn as_psid(&self) -> PSID {
        debug_assert!(size_of_val(self.storage.as_slice()) >= self.byte_len as usize);
        self.storage.as_ptr() as PSID
    }

    fn to_string(&self) -> std::io::Result<String> {
        let mut string_ptr = null_mut();
        // SAFETY: the owned SID is valid and `string_ptr` is an out pointer.
        if unsafe { ConvertSidToStringSidW(self.as_psid(), &mut string_ptr) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let allocation = LocalAllocation::new(string_ptr as HLOCAL)?;
        let mut len = 0;
        // SAFETY: ConvertSidToStringSidW returns a NUL-terminated allocation.
        unsafe {
            while *string_ptr.add(len) != 0 {
                len += 1;
            }
        }
        // SAFETY: the scan above established the initialized UTF-16 slice.
        let wide = unsafe { std::slice::from_raw_parts(string_ptr, len) };
        let sid = OsString::from_wide(wide).to_string_lossy().into_owned();
        drop(allocation);
        Ok(sid)
    }
}

#[cfg(windows)]
struct SecurityDescriptor(LocalAllocation);

#[cfg(windows)]
impl SecurityDescriptor {
    fn for_pipe(sid: &str) -> std::io::Result<Self> {
        // The owner field is explicitly the current user. The protected DACL
        // contains exactly one ACE granting GENERIC_ALL to that SID; Windows
        // requires no SYSTEM or Administrators access-control entry.
        Self::from_sddl(&format!("O:{sid}D:P(A;;GA;;;{sid})"))
    }

    fn for_file(sid: &str) -> std::io::Result<Self> {
        Self::from_sddl(&format!("O:{sid}D:P(A;;GA;;;{sid})"))
    }

    fn for_directory(sid: &str) -> std::io::Result<Self> {
        // OI|CI propagates the same sole current-user ACE to child files and
        // directories; the directory's DACL itself remains protected.
        Self::from_sddl(&format!("O:{sid}D:P(A;OICI;GA;;;{sid})"))
    }

    fn from_sddl(sddl: &str) -> std::io::Result<Self> {
        let wide: Vec<u16> = OsStr::new(sddl).encode_wide().chain(Some(0)).collect();
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: `wide` is NUL terminated and descriptor is a valid out pointer.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self(LocalAllocation::new(descriptor as HLOCAL)?))
    }

    fn as_mut_ptr(&mut self) -> *mut c_void {
        self.0 .0
    }
}

#[cfg(windows)]
struct OwnedHandle(HANDLE);

#[cfg(windows)]
impl OwnedHandle {
    fn new(handle: HANDLE) -> std::io::Result<Self> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }

    fn into_inner(self) -> HANDLE {
        let handle = self.0;
        std::mem::forget(self);
        handle
    }
}

#[cfg(windows)]
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: this RAII wrapper uniquely owns the live Windows handle.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
struct LocalAllocation(HLOCAL);

#[cfg(windows)]
impl LocalAllocation {
    fn new(pointer: HLOCAL) -> std::io::Result<Self> {
        if pointer.is_null() {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Windows API returned a null local allocation",
            ))
        } else {
            Ok(Self(pointer))
        }
    }
}

#[cfg(windows)]
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: this RAII wrapper uniquely owns a LocalAlloc-compatible block.
        unsafe {
            LocalFree(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_listener_is_owner_only_inside_private_directory() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("run").join("incant.sock");
        let endpoint = Endpoint { path: path.clone() };
        let _listener = Listener::bind(&endpoint).unwrap();
        assert_eq!(mode_of(path.parent().unwrap()), 0o700);
        assert_eq!(mode_of(&path), 0o600);
    }

    #[cfg(windows)]
    #[test]
    fn windows_endpoint_contains_current_user_sid() {
        let user = CurrentUser::load().unwrap();
        let endpoint = endpoint().unwrap();
        assert_eq!(
            endpoint.to_string(),
            format!(r"\\.\pipe\incant-{}", user.sid_string)
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_pipe_security_and_owner_are_verified_before_framing() {
        use crate::protocol::framing;
        use tokio::io::AsyncReadExt;
        use windows_sys::Win32::Security::{ImpersonateAnonymousToken, RevertToSelf};
        use windows_sys::Win32::System::Threading::GetCurrentThread;

        let endpoint = endpoint().unwrap();
        let mut listener = Listener::bind(&endpoint).unwrap();
        assert_pipe_is_current_user_only(&listener.next);

        let duplicate = match Listener::bind(&endpoint) {
            Err(error) => error,
            Ok(_) => panic!("duplicate first pipe instance unexpectedly succeeded"),
        };
        assert_eq!(
            duplicate
                .root_cause()
                .downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::raw_os_error),
            Some(windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED as i32)
        );

        let server = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            assert_pipe_is_current_user_only(&stream);
            assert_pipe_is_current_user_only(&listener.next);
            let value: String = framing::read_message(&mut stream).await.unwrap();
            framing::write_message(&mut stream, &value).await.unwrap();
        });
        let mut client = connect(&endpoint).await.unwrap();
        // The well-known Everyone SID (S-1-1-0) is valid but cannot be the
        // accepted owner. This directly exercises the fail-closed verifier.
        let everyone_bytes = [1u8, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0];
        let mut storage = vec![0usize; everyone_bytes.len().div_ceil(size_of::<usize>())];
        // SAFETY: `storage` has at least 12 writable bytes and is SID-aligned.
        unsafe {
            std::ptr::copy_nonoverlapping(
                everyone_bytes.as_ptr(),
                storage.as_mut_ptr() as *mut u8,
                everyone_bytes.len(),
            );
        }
        let everyone = OwnedSid {
            storage,
            byte_len: everyone_bytes.len() as u32,
        };
        let mismatch = verify_pipe_owner(&client, &everyone).unwrap_err();
        assert_eq!(mismatch.kind(), std::io::ErrorKind::PermissionDenied);
        framing::write_message(&mut client, &"secure".to_string())
            .await
            .unwrap();
        let response: String = framing::read_message(&mut client).await.unwrap();
        assert_eq!(response, "secure");
        server.await.unwrap();

        // Build an adversarial live pipe owned by Anonymous while retaining an
        // allow-GA ACE for the current user. A fresh name keeps this phase
        // independent from the still-live first connection. Normal connect
        // must reject the kernel owner before the server can observe even a
        // frame length byte.
        let user = CurrentUser::load().unwrap();
        let wrong_owner_endpoint = Endpoint {
            pipe_name: format!(
                r"\\.\pipe\incant-{}-wrong-owner-{}",
                user.sid_string,
                std::process::id()
            ),
        };
        let mut descriptor =
            SecurityDescriptor::from_sddl(&format!("O:S-1-5-7D:P(A;;GA;;;{})", user.sid_string))
                .unwrap();
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.as_mut_ptr(),
            bInheritHandle: 0,
        };
        let mut options = ServerOptions::new();
        // SAFETY: the pseudo-handle is valid, and attributes points to a live
        // self-relative descriptor for the duration of CreateNamedPipeW.
        assert_ne!(unsafe { ImpersonateAnonymousToken(GetCurrentThread()) }, 0);
        let wrong_owner = unsafe {
            options
                .reject_remote_clients(true)
                .first_pipe_instance(true)
                .create_with_security_attributes_raw(
                    &wrong_owner_endpoint.pipe_name,
                    &mut attributes as *mut _ as *mut std::ffi::c_void,
                )
        };
        // SAFETY: this thread successfully entered anonymous impersonation.
        assert_ne!(unsafe { RevertToSelf() }, 0);
        let mut wrong_owner = wrong_owner.unwrap();

        let observer = tokio::spawn(async move {
            wrong_owner.connect().await.unwrap();
            let mut byte = [0; 1];
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                wrong_owner.read(&mut byte),
            )
            .await
            .expect("wrong-owner client did not disconnect")
            .unwrap()
        });
        tokio::task::yield_now().await;
        let mismatch = connect(&wrong_owner_endpoint).await.unwrap_err();
        assert!(mismatch
            .to_string()
            .contains("Named-pipe server owner is not the current user"));
        assert_eq!(
            mismatch
                .root_cause()
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(std::io::ErrorKind::PermissionDenied)
        );
        assert_eq!(
            observer.await.unwrap(),
            0,
            "client sent framing bytes before rejecting the wrong owner"
        );
    }

    #[cfg(windows)]
    fn assert_current_user_only_descriptor(
        descriptor: PSECURITY_DESCRIPTOR,
        owner: PSID,
        dacl: *mut windows_sys::Win32::Security::ACL,
        directory: bool,
    ) {
        use windows_sys::Win32::Security::{
            AclSizeInformation, GetAce, GetAclInformation, GetSecurityDescriptorControl,
            ACCESS_ALLOWED_ACE, ACL_SIZE_INFORMATION, CONTAINER_INHERIT_ACE, OBJECT_INHERIT_ACE,
            SE_DACL_PROTECTED,
        };
        use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

        let user = CurrentUser::load().unwrap();
        assert!(!owner.is_null());
        assert!(!dacl.is_null());
        // SAFETY: owner points inside the live descriptor allocation.
        assert_ne!(unsafe { EqualSid(owner, user.sid.as_psid()) }, 0);

        let mut control = 0;
        let mut revision = 0;
        // SAFETY: descriptor is live and both output pointers are writable.
        assert_ne!(
            unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) },
            0
        );
        assert_ne!(control & SE_DACL_PROTECTED, 0);

        let mut info = ACL_SIZE_INFORMATION::default();
        // SAFETY: dacl points inside the live descriptor and info is writable.
        assert_ne!(
            unsafe {
                GetAclInformation(
                    dacl,
                    &mut info as *mut ACL_SIZE_INFORMATION as *mut c_void,
                    size_of::<ACL_SIZE_INFORMATION>() as u32,
                    AclSizeInformation,
                )
            },
            0
        );
        assert_eq!(info.AceCount, 1);
        let mut raw_ace: *mut c_void = null_mut();
        // SAFETY: the ACL reports one ACE at index zero.
        assert_ne!(unsafe { GetAce(dacl, 0, &mut raw_ace) }, 0);
        // SAFETY: the only ACE was authored as ACCESS_ALLOWED_ACE in our SDDL.
        let ace = unsafe { &*(raw_ace as *const ACCESS_ALLOWED_ACE) };
        assert_eq!(ace.Header.AceType, 0);
        // The object manager maps SDDL GA to the file/pipe-specific all-access
        // mask when the descriptor is attached to a kernel object.
        assert_eq!(ace.Mask, FILE_ALL_ACCESS);
        let trustee = std::ptr::addr_of!(ace.SidStart) as PSID;
        // SAFETY: SidStart begins the variable-length SID within this ACE.
        assert_ne!(unsafe { EqualSid(trustee, user.sid.as_psid()) }, 0);
        let inherit_mask = (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) as u8;
        let inheritance = ace.Header.AceFlags & inherit_mask;
        if directory {
            assert_eq!(inheritance, inherit_mask);
        } else {
            assert_eq!(inheritance, 0);
        }
    }

    #[cfg(windows)]
    fn assert_pipe_is_current_user_only(stream: &NamedPipeServer) {
        let mut owner: PSID = null_mut();
        let mut dacl = null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: all output pointers are valid and the pipe handle is live.
        let status = unsafe {
            GetSecurityInfo(
                stream.as_raw_handle() as HANDLE,
                SE_KERNEL_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(status, ERROR_SUCCESS);
        let _descriptor = LocalAllocation::new(descriptor as HLOCAL).unwrap();
        assert_current_user_only_descriptor(descriptor, owner, dacl, false);
    }

    #[cfg(windows)]
    fn assert_current_user_only_acl(path: &Path, directory: bool) {
        use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};

        let wide = wide_path(path).unwrap();
        let mut owner: PSID = null_mut();
        let mut dacl = null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: all output pointers are valid and the path is NUL terminated.
        let status = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(status, ERROR_SUCCESS);
        let _descriptor = LocalAllocation::new(descriptor as HLOCAL).unwrap();
        assert_current_user_only_descriptor(descriptor, owner, dacl, directory);
    }

    #[cfg(windows)]
    #[test]
    fn windows_private_files_and_directories_have_only_current_user_ace() {
        let temp = tempfile::tempdir().unwrap();
        let private = temp.path().join("incant");
        let run = private.join("run");
        let file = run.join("incant.startup");
        write_private_file(&file, b"OK").unwrap();

        assert_current_user_only_acl(&private, true);
        assert_current_user_only_acl(&run, true);
        assert_current_user_only_acl(&file, false);
        assert_eq!(std::fs::read(&file).unwrap(), b"OK");
    }
}
