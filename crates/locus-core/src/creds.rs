//! Passwords live only in Windows Credential Manager (generic credentials,
//! one per saved connection), through direct Win32 calls. Target name is
//! `Locus/connection/<id>`, the blob is the raw UTF-8 password, persistence is
//! `CRED_PERSIST_LOCAL_MACHINE`. The `keyring` crate was tried and dropped:
//! it writes `CRED_PERSIST_ENTERPRISE`, which roams the secret with a domain
//! profile to other machines. Nothing here logs or returns the secret in an
//! error message.

use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, FILETIME, GetLastError};
use windows_sys::Win32::Security::Credentials::{
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW,
    CredWriteW,
};

fn target(conn_id: &str) -> String {
    format!("Locus/connection/{conn_id}")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Win32 error code of a failed call.
#[derive(Debug, Clone, Copy)]
pub struct CredError(pub u32);

impl std::fmt::Display for CredError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Win32 error {}", self.0)
    }
}

fn write_target(target: &str, user: &str, secret: &str) -> Result<(), CredError> {
    let mut target = wide(target);
    let mut user = wide(user);
    let mut blob = secret.as_bytes().to_vec();
    let cred = CREDENTIALW {
        Flags: 0,
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        Comment: null_mut(),
        LastWritten: FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        },
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        AttributeCount: 0,
        Attributes: null_mut(),
        TargetAlias: null_mut(),
        UserName: user.as_mut_ptr(),
    };
    // SAFETY: `cred` points at NUL-terminated / sized buffers (`target`, `user`,
    // `blob`) that stay alive and unmoved until the call returns; the API only
    // reads them.
    let ok = unsafe { CredWriteW(&cred, 0) };
    blob.fill(0);
    if ok == 0 {
        // SAFETY: no pointers; reads the calling thread's last error.
        Err(CredError(unsafe { GetLastError() }))
    } else {
        Ok(())
    }
}

fn read_target(target: &str) -> Result<Option<String>, CredError> {
    let target = wide(target);
    let mut p: *mut CREDENTIALW = null_mut();
    // SAFETY: `target` is NUL-terminated; on success `p` receives a buffer we free below.
    let ok = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut p) };
    if ok == 0 {
        // SAFETY: no pointers; reads the calling thread's last error.
        let e = unsafe { GetLastError() };
        return if e == ERROR_NOT_FOUND {
            Ok(None)
        } else {
            Err(CredError(e))
        };
    }
    // SAFETY: `ok != 0`, so `p` points at a valid CREDENTIALW until `CredFree`; the blob
    // pointer/size pair comes from it and is copied out before the free.
    let secret = unsafe {
        let c = &*p;
        let bytes = if c.CredentialBlob.is_null() {
            &[][..]
        } else {
            std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize)
        };
        let s = String::from_utf8_lossy(bytes).into_owned();
        CredFree(p as *const _);
        s
    };
    Ok(Some(secret))
}

fn delete_target(target: &str) -> Result<(), CredError> {
    let target = wide(target);
    // SAFETY: `target` is NUL-terminated and outlives the call.
    let ok = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
    if ok == 0 {
        // SAFETY: no pointers; reads the calling thread's last error.
        let e = unsafe { GetLastError() };
        if e != ERROR_NOT_FOUND {
            return Err(CredError(e));
        }
    }
    Ok(())
}

pub fn write(conn_id: &str, user: &str, secret: &str) -> Result<(), CredError> {
    write_target(&target(conn_id), user, secret)
}

/// `Ok(None)` when no password is stored for the connection.
pub fn read(conn_id: &str) -> Result<Option<String>, CredError> {
    read_target(&target(conn_id))
}

pub fn delete(conn_id: &str) -> Result<(), CredError> {
    delete_target(&target(conn_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let id = format!(
            "selftest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        write(&id, "u", "p@ss 密碼").unwrap();
        assert_eq!(read(&id).unwrap().as_deref(), Some("p@ss 密碼"));
        delete(&id).unwrap();
        assert_eq!(read(&id).unwrap(), None);
        delete(&id).unwrap();
    }

    /// A credential written the way older builds did (direct `CredWriteW`,
    /// UTF-8 blob, local-machine persistence) is read by `read_target`,
    /// and one written by `write_target` is read back by `CredReadW`.
    #[test]
    fn compatible_with_the_old_win32_format() {
        use std::ptr::null_mut;
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::Security::Credentials::{
            CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
            CredWriteW,
        };
        let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() };
        let name = format!(
            "Locus/test/review-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let mut t = wide(&name);
        let mut u = wide("legacy-user");
        let mut blob = "old p@ss 密碼".as_bytes().to_vec();
        let cred = CREDENTIALW {
            Flags: 0,
            Type: CRED_TYPE_GENERIC,
            TargetName: t.as_mut_ptr(),
            Comment: null_mut(),
            LastWritten: FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            },
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: null_mut(),
            TargetAlias: null_mut(),
            UserName: u.as_mut_ptr(),
        };
        // SAFETY: `cred` points at buffers that outlive the call.
        assert_ne!(unsafe { CredWriteW(&cred, 0) }, 0);
        let got = read_target(&name);
        assert_eq!(got.unwrap().as_deref(), Some("old p@ss 密碼"));

        write_target(&name, "new-user", "new secret").unwrap();
        let mut p: *mut CREDENTIALW = null_mut();
        // SAFETY: `t` is NUL-terminated; on success `p` is a credential we free.
        let ok = unsafe { CredReadW(t.as_ptr(), CRED_TYPE_GENERIC, 0, &mut p) };
        assert_ne!(ok, 0);
        // SAFETY: `p` is valid until `CredFree`; blob pointer/size come from it.
        let bytes = unsafe {
            let c = &*p;
            let b = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize)
                .to_vec();
            CredFree(p as *const _);
            b
        };
        assert_eq!(bytes, b"new secret");
        delete_target(&name).unwrap();
        assert_eq!(read_target(&name).unwrap(), None);
    }
}
