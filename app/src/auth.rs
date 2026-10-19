//! System authentication with Windows Hello (face, fingerprint or PIN).

use windows::core::{factory, HSTRING};
use windows::Security::Credentials::UI::{
    UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::IUserConsentVerifierInterop;

#[derive(Debug)]
pub enum AuthError {
    /// Windows Hello is not set up; the person must add a PIN first.
    Unavailable,
    Cancelled,
    Failed,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "windows_hello_unavailable",
            Self::Cancelled => "authentication_cancelled",
            Self::Failed => "authentication_failed",
        })
    }
}

/// Test builds can replace the prompt. Release builds always ask Windows.
fn test_override() -> Option<Result<(), AuthError>> {
    if !cfg!(debug_assertions) {
        return None;
    }
    match std::env::var("ASKKEY_TEST_AUTH").ok()?.as_str() {
        "approve" => Some(Ok(())),
        "cancel" => Some(Err(AuthError::Cancelled)),
        _ => None,
    }
}

/// Shows the Windows Hello prompt over `window` and blocks until the person
/// answers. Call from a worker thread, never the UI thread.
pub fn verify(window: isize, message: &str) -> Result<(), AuthError> {
    if let Some(result) = test_override() {
        return result;
    }
    let availability = UserConsentVerifier::CheckAvailabilityAsync()
        .and_then(|operation| operation.get())
        .map_err(|_| AuthError::Failed)?;
    if availability != UserConsentVerifierAvailability::Available {
        return Err(AuthError::Unavailable);
    }
    let interop = factory::<UserConsentVerifier, IUserConsentVerifierInterop>()
        .map_err(|_| AuthError::Failed)?;
    let result = unsafe {
        interop.RequestVerificationForWindowAsync::<windows_future::IAsyncOperation<UserConsentVerificationResult>>(
            HWND(window as *mut core::ffi::c_void),
            &HSTRING::from(message),
        )
    }
    .and_then(|operation| operation.get())
    .map_err(|_| AuthError::Failed)?;
    match result {
        UserConsentVerificationResult::Verified => Ok(()),
        UserConsentVerificationResult::Canceled => Err(AuthError::Cancelled),
        UserConsentVerificationResult::DeviceNotPresent
        | UserConsentVerificationResult::NotConfiguredForUser => Err(AuthError::Unavailable),
        _ => Err(AuthError::Failed),
    }
}
