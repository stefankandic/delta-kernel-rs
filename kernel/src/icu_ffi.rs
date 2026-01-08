//! Raw ICU 4C FFI bindings and safe wrappers
//!
//! We use ICU 75.1 directly instead of rust_icu to have full control over which
//! ICU version we link against.

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]
#![allow(unreachable_pub)]

// Include the generated bindings
include!(concat!(env!("OUT_DIR"), "/icu_bindings.rs"));

use std::ffi::{CStr, CString};
use std::cmp::Ordering;

/// Get ICU version as "major.minor" string
pub fn get_icu_version() -> String {
    let mut version: [u8; 4] = [0; 4];
    unsafe {
        u_getVersion_75(version.as_mut_ptr());
    }
    format!("{}.{}", version[0], version[1])
}

/// Safe wrapper around UCollator
pub struct Collator {
    ptr: *mut UCollator,
}

impl Collator {
    /// Create a new collator for the given locale
    pub fn try_new(locale: &str) -> Result<Self, String> {
        let locale_cstr = CString::new(locale).map_err(|e| format!("Invalid locale string: {}", e))?;
        let mut status: UErrorCode = 0; // U_ZERO_ERROR = 0

        let ptr = unsafe {
            ucol_open_75(locale_cstr.as_ptr(), &mut status as *mut UErrorCode)
        };

        if status > 0 || ptr.is_null() {
            return Err(format!("Failed to create collator for '{}': error code {}", locale, status));
        }

        Ok(Collator { ptr })
    }

    /// Compare two UTF-8 strings using this collator
    pub fn compare_utf8(&self, s1: &str, s2: &str) -> Result<Ordering, String> {
        let mut status: UErrorCode = 0;

        let result = unsafe {
            ucol_strcollUTF8_75(
                self.ptr,
                s1.as_ptr() as *const i8,
                s1.len() as i32,
                s2.as_ptr() as *const i8,
                s2.len() as i32,
                &mut status as *mut UErrorCode,
            )
        };

        if status > 0 {
            return Err(format!("Collation comparison failed: error code {}", status));
        }

        // UCollationResult: UCOL_LESS = -1, UCOL_EQUAL = 0, UCOL_GREATER = 1
        Ok(match result {
            x if x < 0 => Ordering::Less,
            0 => Ordering::Equal,
            _ => Ordering::Greater,
        })
    }
}

impl Drop for Collator {
    fn drop(&mut self) {
        unsafe {
            ucol_close_75(self.ptr);
        }
    }
}

// Safety: ICU collators are thread-safe for read operations when properly synchronized
unsafe impl Send for Collator {}
unsafe impl Sync for Collator {}

/// Lowercase a single Unicode character using ICU
pub fn to_lower(c: char) -> char {
    let lower = unsafe { u_tolower_75(c as i32) };
    char::from_u32(lower as u32).unwrap_or(c)
}

/// Get list of available locales in Spark format (with 3-letter country codes).
/// Like Java's Locale.getISO3Country(), we convert ICU's 2-letter country codes
/// to 3-letter ISO 3166-1 alpha-3 codes.
pub fn get_available_locales() -> Vec<String> {
    let count = unsafe { ucol_countAvailable_75() };
    let mut locales = Vec::with_capacity(count as usize);

    for i in 0..count {
        unsafe {
            let locale_ptr = ucol_getAvailable_75(i);
            if !locale_ptr.is_null() {
                let locale_cstr = std::ffi::CStr::from_ptr(locale_ptr);
                if let Ok(locale_str) = locale_cstr.to_str() {
                    // Convert ICU format to Spark format (2-letter country -> 3-letter)
                    if let Some(spark_format) = convert_to_spark_format(locale_str) {
                        locales.push(spark_format);
                    }
                }
            }
        }
    }

    locales
}

/// Convert ICU locale format (2-letter country) to Spark format (3-letter country).
/// Returns None if conversion fails.
fn convert_to_spark_format(icu_locale: &str) -> Option<String> {
    let locale_cstr = CString::new(icu_locale).ok()?;

    let mut language_buf = [0i8; 64];
    let mut script_buf = [0i8; 64];
    let mut status: UErrorCode = 0;

    unsafe {
        // Get language code
        let lang_len = uloc_getLanguage_75(
            locale_cstr.as_ptr(),
            language_buf.as_mut_ptr(),
            language_buf.len() as i32,
            &mut status as *mut UErrorCode,
        );

        if status > 0 || lang_len <= 0 {
            return None;
        }

        let language = CStr::from_ptr(language_buf.as_ptr())
            .to_str()
            .ok()?
            .to_string();

        // Get script code (optional)
        status = 0;
        let script_len = uloc_getScript_75(
            locale_cstr.as_ptr(),
            script_buf.as_mut_ptr(),
            script_buf.len() as i32,
            &mut status as *mut UErrorCode,
        );

        let script = if status == 0 && script_len > 0 {
            Some(
                CStr::from_ptr(script_buf.as_ptr())
                    .to_str()
                    .ok()?
                    .to_string(),
            )
        } else {
            None
        };

        // Get 3-letter country code (ISO 3166-1 alpha-3)
        let country_ptr = uloc_getISO3Country_75(locale_cstr.as_ptr());

        let country = if !country_ptr.is_null() {
            let country_str = CStr::from_ptr(country_ptr).to_str().ok()?;
            if !country_str.is_empty() {
                Some(country_str.to_string())
            } else {
                None
            }
        } else {
            None
        };

        // Build Spark format: language[_Script][_Country]
        let mut parts = vec![language];
        if let Some(s) = script {
            parts.push(s);
        }
        if let Some(c) = country {
            parts.push(c);
        }
        Some(parts.join("_"))
    }
}
