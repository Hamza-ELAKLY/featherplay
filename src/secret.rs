//! Keeping a secret on disk so only this Windows account can read it back: Windows'
//! data protection (DPAPI) encrypts it with a key tied to the signed-in user. Copied to
//! another PC or user, the file can't be decrypted.
//!
//! Elsewhere there is no such store yet (the system keyring comes later), so nothing is
//! kept and sign-in isn't offered.

/// Whether secrets can be kept on this system.
pub const AVAILABLE: bool = cfg!(windows);

#[cfg(windows)]
mod dpapi {
    use std::ffi::c_void;

    #[repr(C)]
    struct Blob {
        size: u32,
        data: *mut u8,
    }

    // Shows no prompt; the key is the user's, not the machine's.
    const UI_FORBIDDEN: u32 = 0x1;

    #[link(name = "crypt32")]
    unsafe extern "system" {
        fn CryptProtectData(
            input: *const Blob,
            description: *const u16,
            entropy: *const Blob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            output: *mut Blob,
        ) -> i32;
        fn CryptUnprotectData(
            input: *const Blob,
            description: *mut *mut u16,
            entropy: *const Blob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            output: *mut Blob,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    // Binds the secret to Featherplay too: another program running as the same user
    // calling DPAPI without this can't read it back.
    const ENTROPY: &[u8] = b"featherplay account v1";

    fn run(data: &[u8], protect: bool) -> Result<Vec<u8>, String> {
        let input = Blob { size: data.len() as u32, data: data.as_ptr().cast_mut() };
        let entropy = Blob { size: ENTROPY.len() as u32, data: ENTROPY.as_ptr().cast_mut() };
        let mut output = Blob { size: 0, data: std::ptr::null_mut() };
        let null = std::ptr::null_mut();
        // SAFETY: the blobs point at live buffers for the call's duration; the output is
        // allocated by Windows, copied, then released with LocalFree as documented.
        unsafe {
            let ok = if protect {
                CryptProtectData(&input, std::ptr::null(), &entropy, null, null, UI_FORBIDDEN, &mut output)
            } else {
                CryptUnprotectData(&input, std::ptr::null_mut(), &entropy, null, null, UI_FORBIDDEN, &mut output)
            };
            if ok == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let bytes = std::slice::from_raw_parts(output.data, output.size as usize).to_vec();
            LocalFree(output.data.cast());
            Ok(bytes)
        }
    }

    pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
        run(data, true)
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
        run(data, false)
    }
}

/// Encrypts `data` for this user.
pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
    #[cfg(windows)]
    return dpapi::protect(data);
    #[cfg(not(windows))]
    {
        let _ = data;
        Err("no secret store on this system yet".into())
    }
}

/// Decrypts what `protect` made, if it was made for this user.
pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    #[cfg(windows)]
    return dpapi::unprotect(data);
    #[cfg(not(windows))]
    {
        let _ = data;
        Err("no secret store on this system yet".into())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn a_secret_round_trips_and_is_not_plain_text() {
        let secret = b"1//refresh-token-that-must-not-show";
        let sealed = protect(secret).unwrap();
        assert!(!sealed.windows(secret.len()).any(|w| w == secret), "the text isn't in the file");
        assert_eq!(unprotect(&sealed).unwrap(), secret);
        // Damaged (or another user's) data doesn't decrypt.
        let mut damaged = sealed.clone();
        let last = damaged.len() - 1;
        damaged[last] ^= 0xff;
        assert!(unprotect(&damaged).is_err());
    }
}
