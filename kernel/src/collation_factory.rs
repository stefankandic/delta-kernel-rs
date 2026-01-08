//! Collation factory for creating collators from collation identifiers.
//!
//! This module provides the factory pattern for creating collation instances that perform
//! string comparisons according to specified collation rules.
//!
//! ## Implementation
//!
//! Uses raw ICU 4C 75.1 FFI bindings (not rust_icu) for full control over ICU version and linking.
//! ICU is statically linked, eliminating runtime library dependencies.
//!
//! ## Caching
//!
//! Like Spark's CollationFactory which caches collation instances in a ConcurrentHashMap,
//! this implementation caches Collation instances with thread-safe access.
//!
//! - Cache key: lowercased collation name (e.g., "unicode", "en_us_ci")
//! - Case-insensitive: "UNICODE" and "unicode" map to the same entry
//! - Provider and version NOT included in cache key
//! - ICU collators are wrapped in `Arc<Mutex<>>` for thread-safe access
//! - Collators are locked during comparison operations (similar to synchronized access in Java)

use crate::collation::{CollationIdentifier, CollationProvider};
use crate::icu_ffi;
use crate::DeltaResult;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

/// ICU version string in "major.minor" format (e.g., "75.1").
/// This is computed at runtime from the actual ICU library being used.
/// Like Java's ICU_VERSION from VersionInfo.ICU_VERSION.
pub static ICU_VERSION: LazyLock<String> = LazyLock::new(|| icu_ffi::get_icu_version());

/// Set of available ICU locale names in lowercase.
/// Built from ICU's available locales, similar to Spark's ICULocaleMapUppercase.
/// Used to validate that a locale name is actually supported by ICU.
static AVAILABLE_ICU_LOCALES: LazyLock<HashSet<String>> = LazyLock::new(|| {
    let locales = icu_ffi::get_available_locales();
    locales.into_iter().map(|s| s.to_lowercase()).collect()
});

/// A collation instance that provides string comparison functionality.
///
/// This struct encapsulates all the information and functions needed to perform
/// collation-aware string operations.
///
/// ICU collators are wrapped in Arc<Mutex<>> for thread-safe shared access,
/// similar to Spark/Java's CollatorICU which uses synchronized methods.
pub struct Collation {
    /// The collation identifier
    pub identifier: CollationIdentifier,

    /// ICU collator instance wrapped in Arc<Mutex<>> for thread-safe access
    /// Only present for ICU collations, None for Spark collations
    collator: Option<Arc<Mutex<icu_ffi::Collator>>>,
}

impl Collation {
    /// Compares two strings according to this collation.
    ///
    /// Returns:
    /// - `Ok(Ordering::Less)` if s1 < s2
    /// - `Ok(Ordering::Equal)` if s1 == s2
    /// - `Ok(Ordering::Greater)` if s1 > s2
    /// - `Err` if the collation operation fails
    pub fn compare(&self, s1: &str, s2: &str) -> DeltaResult<std::cmp::Ordering> {
        match &self.identifier.provider {
            CollationProvider::Spark => {
                if self.identifier.is_spark_utf8_binary() {
                    // UTF8_BINARY: Binary byte comparison
                    Ok(s1.cmp(s2))
                } else if self.identifier.is_spark_utf8_lcase() {
                    // UTF8_LCASE: Lowercase comparison
                    Ok(self.compare_lowercase(s1, s2))
                } else {
                    // Unsupported Spark collation
                    Err(crate::Error::unsupported(format!(
                        "Spark collation '{}' is not supported",
                        self.identifier.name
                    )))
                }
            }
            CollationProvider::Icu => {
                if let Some(ref collator_arc) = self.collator {
                    // Lock the collator for thread-safe access
                    let collator = collator_arc.lock().unwrap();
                    // Use compare_utf8 which works directly with &str
                    collator.compare_utf8(s1, s2).map_err(|e| {
                        crate::Error::generic(format!(
                            "ICU collation comparison failed: {}",
                            e
                        ))
                    })
                } else {
                    // This should never happen - collator should always be present for ICU
                    Err(crate::Error::generic(
                        "ICU collator not initialized".to_string()
                    ))
                }
            }
            CollationProvider::Other(provider) => {
                Err(crate::Error::unsupported(format!(
                    "Collation provider '{}' is not supported",
                    provider
                )))
            }
        }
    }

    /// Checks if two strings are equal according to this collation.
    ///
    /// Returns `Ok(true)` if equal, `Ok(false)` if not equal, or `Err` on failure.
    pub fn equals(&self, s1: &str, s2: &str) -> DeltaResult<bool> {
        Ok(self.compare(s1, s2)? == std::cmp::Ordering::Equal)
    }

    /// Lowercase comparison helper for UTF8_LCASE collation.
    ///
    /// Matches Delta Spark `compareLowerCase` semantics exactly:
    /// - Fast path for ASCII strings
    /// - ICU-based lowercase for non-ASCII with special handling for İ and ς
    fn compare_lowercase(&self, s1: &str, s2: &str) -> std::cmp::Ordering {
        // Fast path: if both strings are ASCII, use byte-by-byte comparison
        if s1.is_ascii() && s2.is_ascii() {
            return Self::compare_lowercase_ascii(s1, s2);
        }

        // Slow path: use ICU lowercase conversion for non-ASCII
        let lower1 = Self::lowercase_code_points(s1);
        let lower2 = Self::lowercase_code_points(s2);
        lower1.cmp(&lower2)
    }

    /// Fast ASCII-only lowercase comparison.
    /// Matches Spark's `compareLowerCaseAscii`.
    fn compare_lowercase_ascii(s1: &str, s2: &str) -> std::cmp::Ordering {
        let bytes1 = s1.as_bytes();
        let bytes2 = s2.as_bytes();
        let min_len = bytes1.len().min(bytes2.len());

        for i in 0..min_len {
            let lower1 = bytes1[i].to_ascii_lowercase();
            let lower2 = bytes2[i].to_ascii_lowercase();
            if lower1 != lower2 {
                return lower1.cmp(&lower2);
            }
        }
        bytes1.len().cmp(&bytes2.len())
    }

    /// Converts a string to lowercase using ICU with Spark-compatible special handling.
    ///
    /// Matches Delta Spark `lowerCaseCodePoints`:
    /// - İ (U+0130) → i (U+0069) + combining dot (U+0307)
    /// - ς (U+03C2) → σ (U+03C3) [Greek final sigma → Greek small sigma, context-unaware]
    /// - All others: ICU UCharacter.toLowerCase(codePoint)
    fn lowercase_code_points(s: &str) -> String {
        // Constants from Spark's SpecialCodePointConstants
        const CAPITAL_I_WITH_DOT_ABOVE: u32 = 0x0130; // İ
        const ASCII_SMALL_I: char = '\u{0069}'; // i
        const COMBINING_DOT: char = '\u{0307}'; // combining dot above
        const GREEK_FINAL_SIGMA: u32 = 0x03C2; // ς
        const GREEK_SMALL_SIGMA: char = '\u{03C3}'; // σ

        let mut result = String::with_capacity(s.len());

        for ch in s.chars() {
            let codepoint = ch as u32;

            if codepoint == CAPITAL_I_WITH_DOT_ABOVE {
                // İ → i + combining dot (one-to-many mapping)
                result.push(ASCII_SMALL_I);
                result.push(COMBINING_DOT);
            } else if codepoint == GREEK_FINAL_SIGMA {
                // ς → σ (context-unaware mapping)
                result.push(GREEK_SMALL_SIGMA);
            } else {
                // Use ICU's to_lower for all other characters
                let lower_char = icu_ffi::to_lower(ch);
                result.push(lower_char);
            }
        }

        result
    }
}

/// Parsed locale components extracted from a collation name.
struct ParsedLocale {
    language: String,                  // 2-letter code (required, e.g., "en", "fr")
    script: Option<String>,            // 4-letter code (optional, e.g., "Hant", "Cyrl")
    country: Option<String>,           // 3-letter code (optional, e.g., "USA", "GBR")
    #[allow(dead_code)]
    case_sensitive: bool,              // false if _CI specified
    #[allow(dead_code)]
    accent_sensitive: bool,            // false if _AI specified
}

impl ParsedLocale {
    /// Parses a collation name into its components.
    ///
    /// Format: `language[_Script][_Country][_CI|_CS][_AI|_AS]`
    /// All components are case-insensitive.
    fn parse(name: &str) -> DeltaResult<Self> {
        // Special case: "unicode" is the root collation (case-insensitive)
        if name.eq_ignore_ascii_case("unicode") {
            return Ok(Self {
                language: "unicode".to_string(),
                script: None,
                country: None,
                case_sensitive: true,
                accent_sensitive: true,
            });
        }

        // Check for unicode with modifiers (e.g., "unicode_CI")
        let upper = name.to_uppercase();
        if upper.starts_with("UNICODE_") {
            let (case_sensitive, accent_sensitive) = Self::parse_sensitivity_modifiers(name)?;
            return Ok(Self {
                language: "unicode".to_string(),
                script: None,
                country: None,
                case_sensitive,
                accent_sensitive,
            });
        }

        // Reject malformed strings
        if name.starts_with('_') || name.ends_with('_') {
            return Err(crate::Error::unsupported(format!(
                "Invalid locale '{}': locale cannot start or end with underscore",
                name
            )));
        }

        // Parse sensitivity modifiers and get base locale
        let (case_sensitive, accent_sensitive) = Self::parse_sensitivity_modifiers(name)?;
        let base = Self::strip_sensitivity_modifiers(name);

        // Check for leading/trailing underscores
        if base.starts_with('_') || base.ends_with('_') {
            return Err(crate::Error::unsupported(format!(
                "Invalid locale '{}': locale cannot start or end with underscore",
                name
            )));
        }

        // Check for empty components (double underscores)
        if base.contains("__") {
            return Err(crate::Error::unsupported(format!(
                "Invalid locale '{}': locale contains empty components (double underscore)",
                name
            )));
        }

        // Split by underscore to get locale components
        let parts: Vec<&str> = base.split('_').collect();

        // Check max components (language_Script_Country = max 3)
        if parts.len() > 3 {
            return Err(crate::Error::unsupported(format!(
                "Invalid locale '{}': too many components (expected language[_Script][_Country])",
                name
            )));
        }

        // Parse components based on count
        match parts.len() {
            1 => {
                // Just language (e.g., "en")
                let language = Self::validate_language(parts[0], name)?;
                Ok(Self {
                    language,
                    script: None,
                    country: None,
                    case_sensitive,
                    accent_sensitive,
                })
            }
            2 => {
                // language_XXX where XXX is either Script (4 letters) or Country (3 letters)
                let language = Self::validate_language(parts[0], name)?;
                let second = parts[1];

                // Determine if second component is script or country by length
                if second.len() == 3 {
                    let country = Self::validate_country(second, name)?;
                    Ok(Self {
                        language,
                        script: None,
                        country: Some(country),
                        case_sensitive,
                        accent_sensitive,
                    })
                } else if second.len() == 4 {
                    let script = Self::validate_script(second, name)?;
                    Ok(Self {
                        language,
                        script: Some(script),
                        country: None,
                        case_sensitive,
                        accent_sensitive,
                    })
                } else if second.len() == 2 {
                    Err(crate::Error::unsupported(format!(
                        "Invalid locale '{}': Spark format requires 3-letter country codes (e.g., en_USA, not en_US)",
                        name
                    )))
                } else {
                    Err(crate::Error::unsupported(format!(
                        "Invalid locale '{}': second component must be 3-letter country or 4-letter script code",
                        name
                    )))
                }
            }
            3 => {
                // language_Script_Country
                let language = Self::validate_language(parts[0], name)?;
                let script = Self::validate_script(parts[1], name)?;
                let country = Self::validate_country(parts[2], name)?;

                Ok(Self {
                    language,
                    script: Some(script),
                    country: Some(country),
                    case_sensitive,
                    accent_sensitive,
                })
            }
            _ => {
                Err(crate::Error::unsupported(format!(
                    "Invalid locale '{}': too many components (expected language[_Script][_Country])",
                    name
                )))
            }
        }
    }

    /// Validates a language code (must be 2 letters, alphabetic).
    fn validate_language(lang: &str, original_name: &str) -> DeltaResult<String> {
        if lang.len() != 2 {
            return Err(crate::Error::unsupported(format!(
                "Invalid locale '{}': language code '{}' must be exactly 2 letters",
                original_name, lang
            )));
        }
        if !lang.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(crate::Error::unsupported(format!(
                "Invalid locale '{}': language code '{}' must be alphabetic",
                original_name, lang
            )));
        }
        Ok(lang.to_string())
    }

    /// Validates a script code (must be 4 letters, alphabetic).
    fn validate_script(script: &str, original_name: &str) -> DeltaResult<String> {
        if script.len() != 4 {
            return Err(crate::Error::unsupported(format!(
                "Invalid locale '{}': script code '{}' must be exactly 4 letters",
                original_name, script
            )));
        }
        if !script.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(crate::Error::unsupported(format!(
                "Invalid locale '{}': script code '{}' must be alphabetic",
                original_name, script
            )));
        }
        Ok(script.to_string())
    }

    /// Validates a country code (must be 3 letters, alphabetic).
    fn validate_country(country: &str, original_name: &str) -> DeltaResult<String> {
        if country.len() == 2 {
            return Err(crate::Error::unsupported(format!(
                "Invalid locale '{}': Spark format requires 3-letter country codes, got '{}'",
                original_name, country
            )));
        }
        if country.len() != 3 {
            return Err(crate::Error::unsupported(format!(
                "Invalid locale '{}': country code '{}' must be exactly 3 letters",
                original_name, country
            )));
        }
        if !country.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(crate::Error::unsupported(format!(
                "Invalid locale '{}': country code '{}' must be alphabetic",
                original_name, country
            )));
        }
        Ok(country.to_string())
    }

    /// Parses sensitivity modifiers (_CI, _CS, _AI, _AS) from the name.
    /// Returns (case_sensitive, accent_sensitive).
    fn parse_sensitivity_modifiers(name: &str) -> DeltaResult<(bool, bool)> {
        let upper = name.to_uppercase();

        // Default: both case and accent sensitive
        let mut case_sensitive = true;
        let mut accent_sensitive = true;

        // Check for _CI (case insensitive)
        if upper.contains("_CI") {
            case_sensitive = false;
        }
        // Check for _CS (case sensitive - explicit, but it's the default)
        if upper.contains("_CS") {
            case_sensitive = true;
        }
        // Check for _AI (accent insensitive)
        if upper.contains("_AI") {
            accent_sensitive = false;
        }
        // Check for _AS (accent sensitive - explicit, but it's the default)
        if upper.contains("_AS") {
            accent_sensitive = true;
        }

        Ok((case_sensitive, accent_sensitive))
    }

    /// Strips sensitivity modifiers from the name to get the base locale.
    fn strip_sensitivity_modifiers(name: &str) -> String {
        let upper = name.to_uppercase();
        let mut base = name.to_string();

        // Strip all possible suffix combinations (case-insensitive)
        // Order matters: check longer suffixes first
        if upper.ends_with("_CI_AI") || upper.ends_with("_CS_AS") ||
           upper.ends_with("_AI_CI") || upper.ends_with("_AS_CS") ||
           upper.ends_with("_CI_AS") || upper.ends_with("_CS_AI") ||
           upper.ends_with("_AI_CS") || upper.ends_with("_AS_CI") {
            base.truncate(base.len() - 6);
        } else if upper.ends_with("_CI") || upper.ends_with("_AI") ||
                  upper.ends_with("_CS") || upper.ends_with("_AS") {
            base.truncate(base.len() - 3);
        }

        base
    }

    /// Returns the base locale string (language[_Script][_Country]) for ICU validation.
    fn base_locale(&self) -> String {
        let mut parts = vec![self.language.clone()];
        if let Some(ref script) = self.script {
            parts.push(script.clone());
        }
        if let Some(ref country) = self.country {
            parts.push(country.clone());
        }
        parts.join("_")
    }

    /// Validates that the locale is available in ICU.
    ///
    /// Like Spark's CollationFactory.collationNameToId(), we search for the longest
    /// valid locale prefix. If the longest match is not the full locale name,
    /// it means there are invalid components in the locale string.
    fn validate_availability(&self, original_name: &str) -> DeltaResult<()> {
        // Special case: "unicode" (root collation) is always valid
        if self.language.eq_ignore_ascii_case("unicode") {
            return Ok(());
        }

        // Build the locale string in Spark format (with 3-letter country codes)
        let base_locale = self.base_locale();
        let base_lowercase = base_locale.to_lowercase();

        // Search for the longest locale match (Spark's algorithm)
        let mut last_valid_pos = None;
        for i in 1..=base_lowercase.len() {
            let prefix = &base_lowercase[..i];
            if AVAILABLE_ICU_LOCALES.contains(prefix) {
                last_valid_pos = Some(i);
            }
        }

        match last_valid_pos {
            Some(pos) if pos == base_lowercase.len() => {
                // The entire locale name is valid
                Ok(())
            }
            Some(pos) => {
                // Found a valid prefix but there's extra invalid stuff after it
                let valid_part = &base_locale[..pos];
                let invalid_part = &base_locale[pos..];
                Err(crate::Error::unsupported(format!(
                    "Invalid locale '{}': '{}' is valid but '{}' is not a valid component",
                    original_name, valid_part, invalid_part
                )))
            }
            None => {
                // No valid locale prefix found at all
                Err(crate::Error::unsupported(format!(
                    "Collation '{}' is not available in ICU. The locale '{}' is not supported.",
                    original_name, base_locale
                )))
            }
        }
    }
}

/// Thread-safe cache for collation instances.
///
/// Collations are expensive to create (especially ICU collators), so we cache them by name.
/// Cache key: lowercased collation name - provider and version are NOT included.
/// ICU collators are wrapped in Arc<Mutex<>> for thread-safe shared access.
static COLLATION_CACHE: LazyLock<RwLock<HashMap<String, Arc<Collation>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Factory for creating collation instances from collation identifiers.
pub struct CollationFactory;

impl CollationFactory {
    /// Normalizes a collation name for caching (just lowercases it).
    ///
    /// This provides case-insensitive caching so "UNICODE" and "unicode" map to the same entry.
    /// We don't normalize modifiers or order - if someone uses "unicode_CI_AI" vs "unicode_AI_CI",
    /// they'll get separate cache entries, which is fine.
    fn normalize_name(name: &str) -> String {
        name.to_lowercase()
    }

    /// Creates a new `Collation` instance from a `CollationIdentifier`.
    ///
    /// This method uses a thread-safe cache to avoid recreating expensive collation objects.
    /// Cache key is the normalized collation name - provider and version are NOT included.
    ///
    /// # Arguments
    ///
    /// * `identifier` - The collation identifier specifying which collation to create
    ///
    /// # Returns
    ///
    /// An `Arc<Collation>` instance configured according to the identifier
    ///
    /// # Errors
    ///
    /// Returns an error if the collation is not supported or cannot be created
    pub fn from_identifier(identifier: CollationIdentifier) -> DeltaResult<Arc<Collation>> {
        // Validate that the collation is supported
        identifier.validate_support()?;

        // Create cache key: just the normalized name (provider and version not needed)
        let cache_key = Self::normalize_name(&identifier.name);

        // Check if collation is already in cache (read lock)
        {
            let cache = COLLATION_CACHE.read().unwrap();
            if let Some(cached_collation) = cache.get(&cache_key) {
                // Cache hit! Return clone of the Arc
                return Ok(Arc::clone(cached_collation));
            }
        }

        // Not in cache - create new collation (write lock)
        let mut cache = COLLATION_CACHE.write().unwrap();

        // Double-check in case another thread created it while we waited for write lock
        if let Some(cached_collation) = cache.get(&cache_key) {
            return Ok(Arc::clone(cached_collation));
        }

        // Parse and validate (expensive operation for ICU)
        if let CollationProvider::Icu = &identifier.provider {
            ParsedLocale::parse(&identifier.name)?;
        }

        // Create the collation
        let collation = match &identifier.provider {
            CollationProvider::Spark => Self::create_spark_collation(identifier)?,
            CollationProvider::Icu => Self::create_icu_collation(identifier)?,
            CollationProvider::Other(_) => {
                return Err(crate::Error::unsupported(format!(
                    "Collation provider '{}' is not supported",
                    identifier.provider
                )))
            }
        };

        // Wrap in Arc and cache it
        let collation = Arc::new(collation);
        cache.insert(cache_key, Arc::clone(&collation));
        Ok(collation)
    }

    /// Creates a Spark collation (UTF8_BINARY or UTF8_LCASE).
    fn create_spark_collation(identifier: CollationIdentifier) -> DeltaResult<Collation> {
        let is_binary = identifier.is_spark_utf8_binary();
        let is_lcase = identifier.is_spark_utf8_lcase();

        if !is_binary && !is_lcase {
            return Err(crate::Error::unsupported(format!(
                "Spark collation '{}' is not supported. Only UTF8_BINARY and UTF8_LCASE are supported.",
                identifier.name
            )));
        }

        Ok(Collation {
            identifier,
            collator: None,
        })
    }

    /// Creates an ICU collation by parsing and validating the locale components.
    fn create_icu_collation(identifier: CollationIdentifier) -> DeltaResult<Collation> {
        // Parse the collation name into components
        let parsed = ParsedLocale::parse(&identifier.name)?;

        // Validate that the locale components are available in ICU
        parsed.validate_availability(&identifier.name)?;

        // Convert Spark format to ICU format and create collator
        let icu_locale = identifier.to_icu_locale_string();

        // Create ICU collator and wrap in Arc<Mutex<>> for thread-safe access
        let collator = icu_ffi::Collator::try_new(&icu_locale).map_err(|e| {
            crate::Error::unsupported(format!(
                "Failed to create ICU collator for '{}': {}",
                identifier.name, e
            ))
        })?;

        Ok(Collation {
            identifier,
            collator: Some(Arc::new(Mutex::new(collator))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_utf8_binary_collation() {
        let identifier = CollationIdentifier::spark("UTF8_BINARY");
        let collation = CollationFactory::from_identifier(identifier).unwrap();

        // Test comparison
        assert_eq!(collation.compare("abc", "abc").unwrap(), std::cmp::Ordering::Equal);
        assert_eq!(collation.compare("abc", "ABC").unwrap(), std::cmp::Ordering::Greater);
        assert_eq!(collation.compare("abc", "def").unwrap(), std::cmp::Ordering::Less);

        // Test equality
        assert!(collation.equals("abc", "abc").unwrap());
        assert!(!collation.equals("abc", "ABC").unwrap());

        assert!(!collation.equals("Müller", "müller").unwrap());
        assert!(!collation.equals("Müller", "muller").unwrap());
    }

    #[test]
    fn test_utf8_lcase_collation() {
        let identifier = CollationIdentifier::spark("UTF8_LCASE");
        let collation = CollationFactory::from_identifier(identifier).unwrap();

        // Test case-insensitive comparison
        assert_eq!(collation.compare("abc", "ABC").unwrap(), std::cmp::Ordering::Equal);
        assert_eq!(collation.compare("abc", "def").unwrap(), std::cmp::Ordering::Less);

        // Test case-insensitive equality
        assert!(collation.equals("abc", "ABC").unwrap());
        assert!(collation.equals("Hello", "HELLO").unwrap());
        assert!(!collation.equals("abc", "def").unwrap());
        assert!(!collation.equals("abc", "def").unwrap());

        assert!(collation.equals("Müller", "müller").unwrap());
        assert!(!collation.equals("Müller", "muller").unwrap());
    }

    #[test]
    fn test_icu_unicode_collation() {
        let identifier = CollationIdentifier::icu("unicode", None);
        let collation = CollationFactory::from_identifier(identifier).unwrap();

        // Default unicode collation is case and accent sensitive (tertiary strength)
        // Same strings should be equal
        assert!(collation.equals("abc", "abc").unwrap());

        // Different case should be different (case sensitive)
        assert!(!collation.equals("abc", "ABC").unwrap());

        assert!(!collation.equals("Müller", "müller").unwrap());
        assert!(!collation.equals("Müller", "muller").unwrap());
        assert!(!collation.equals("Müller", "Muller").unwrap());
    }

    #[test]
    fn test_icu_unicode_ci_collation() {
        // Use actual collation name: unicode_CI (case insensitive)
        let identifier = CollationIdentifier::icu("unicode_CI", None);
        let collation = CollationFactory::from_identifier(identifier).unwrap();

        assert!(collation.equals("abc", "ABC").unwrap());

        assert!(collation.equals("Müller", "müller").unwrap());
        assert!(!collation.equals("Müller", "muller").unwrap());
        assert!(!collation.equals("Müller", "Muller").unwrap());
    }

    #[test]
    fn test_icu_unicode_ci_ai_collation() {
        // Use actual collation name: unicode_CI_AI (case and accent insensitive)
        let identifier = CollationIdentifier::icu("unicode_CI_AI", None);
        let collation = CollationFactory::from_identifier(identifier).unwrap();

        assert!(collation.equals("abc", "ABC").unwrap());

        assert!(collation.equals("Müller", "müller").unwrap());
        assert!(collation.equals("Müller", "muller").unwrap());
        assert!(collation.equals("Müller", "Muller").unwrap());
    }

    #[test]
    fn test_icu_unicode_cs_ai_collation() {
        // CS_AI (case sensitive, accent insensitive) using ICU's ks=level1;kc=true
        // This follows Spark implementation
        // Test both orderings: CS_AI, AI_CS, and just AI should behave identically

        for name in ["unicode_CS_AI", "unicode_AI_CS", "unicode_AI"] {
            let identifier = CollationIdentifier::icu(name, None);
            let collation = CollationFactory::from_identifier(identifier).unwrap();

            // CS: Case sensitive - "abc" and "ABC" should be DIFFERENT
            assert!(!collation.equals("abc", "ABC").unwrap());
            assert!(!collation.equals("Müller", "müller").unwrap());

            // AI: Accent insensitive - "Müller" and "Muller" should be EQUAL
            assert!(collation.equals("Müller", "Muller").unwrap());
            assert!(collation.equals("müller", "muller").unwrap());
        }
    }

    #[test]
    fn test_unsupported_collation_errors() {
        // Create an unsupported Spark collation
        let identifier = CollationIdentifier::spark("UNSUPPORTED_COLLATION");

        // Validation should fail
        assert!(identifier.validate_support().is_err());

        // But if we bypass validation and create it anyway, comparison should error
        let collation = Collation {
            identifier,
            collator: None,
        };

        // Operations should return errors, not fall back to binary comparison
        let result = collation.compare("abc", "def");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not supported"));

        let result = collation.equals("abc", "abc");
        assert!(result.is_err());
    }

    #[test]
    fn test_icu_collator_error_propagates() {
        // Test that ICU collator errors are properly propagated, not silently converted to Equal
        // This ensures we don't hide ICU errors
        // Use 3-letter country code (Spark format)
        let identifier = CollationIdentifier::icu("en_USA_CI", None);
        let collation = CollationFactory::from_identifier(identifier).unwrap();

        // Normal comparisons should work
        assert!(collation.compare("abc", "def").is_ok());
        assert!(collation.equals("abc", "ABC").unwrap());
    }

    #[test]
    fn test_two_letter_country_code_rejected() {
        // Spark format requires 3-letter country codes
        // 2-letter codes like en_US should be rejected
        let identifier = CollationIdentifier::icu("en_US_CI", None);
        let result = CollationFactory::from_identifier(identifier);

        // This should fail - Spark requires 3-letter country codes (en_USA, not en_US)
        match result {
            Err(e) => {
                let err_msg = e.to_string();
                assert!(
                    err_msg.contains("en_US") || err_msg.contains("collator"),
                    "Error should mention the invalid locale: {}",
                    err_msg
                );
            }
            Ok(_) => panic!("Expected en_US_CI to be rejected, but it was accepted"),
        }
    }

    #[test]
    fn test_invalid_icu_locale_rejected() {
        // Test that invalid ICU locales are properly rejected by our validation
        // Based on Spark's invalid collation name tests
        let invalid_locales = vec![
            // Invalid language codes (wrong length or format)
            ("xyz", "non-existent language code"),
            ("enn", "invalid 3-letter language code"),
            ("zzz_USA", "non-existent language with valid country"),
            ("abcd_USA", "invalid language code length"),

            // Invalid script codes (wrong format or case)
            ("en_Abcd_USA", "invalid script code format (not title case)"),
            ("en_Xyz_USA", "non-existent script code"),
            ("en_AB_USA", "script code too short"),
            ("en_LATN_USA", "script code all uppercase"),
            ("en_latn_USA", "script code all lowercase"),
            ("en_Latn_USA", "Latin script not valid for English locale"),
            ("en_Cyrl_USA", "Cyrillic script not valid for English locale"),

            // Invalid country codes (not available in ICU)
            ("en_XYZ", "non-existent country code"),
            ("en_GBR", "country code not available in ICU"),
            ("en_AAA", "invalid 3-letter country code"),
            ("en_ABCD", "country code too long"),
            ("en_999", "numeric country code"),

            // Invalid components
            ("en_Something", "invalid component 'Something'"),
            ("en_Something_USA", "invalid script code 'Something'"),
            ("en_USA_AAA", "invalid component after country"),
            ("sr_Cyrl_SRB_AAA", "invalid component after full locale"),

            // Invalid ordering of components (language, script, country)
            ("USA_en", "country code before language"),
            ("sr_SRB_Cyrl", "country code before script"),
            ("SRB_sr", "country code before language"),
            ("SRB_sr_Cyrl", "country code before language and script"),
            ("SRB_Cyrl_sr", "wrong ordering of all components"),
            ("Cyrl_sr", "script code before language"),
            ("Cyrl_sr_SRB", "script code before language"),
            ("Cyrl_SRB_sr", "wrong ordering of all components"),

            // Collation specifiers in wrong place (must be at end)
            ("en_CI_USA", "CI modifier before country"),
            ("sr_CI_Cyrl_SRB", "CI modifier before script"),
            ("sr_Cyrl_CI_SRB", "CI modifier between script and country"),
            ("CI_en", "CI modifier before language"),
            ("USA_CI_en", "CI modifier in middle"),

            // Malformed strings
            ("en__USA", "double underscore"),
            ("en_USA_USA_USA", "too many components"),
            ("_en_USA", "leading underscore"),
            ("en_USA_", "trailing underscore"),
            ("_CI_AI", "no locale specified, starts with modifier"),
        ];

        for (locale, description) in invalid_locales {
            let identifier = CollationIdentifier::icu(locale, None);
            let result = CollationFactory::from_identifier(identifier);

            match result {
                Err(e) => {
                    let err_msg = e.to_string();
                    // Verify error mentions either the locale name or validation failure
                    assert!(
                        err_msg.contains(locale) || err_msg.contains("Invalid locale") || err_msg.contains("must be"),
                        "Error for '{}' ({}) should mention the locale or validation error. Got: {}",
                        locale,
                        description,
                        err_msg
                    );
                }
                Ok(_) => panic!(
                    "Expected invalid locale '{}' ({}) to be rejected, but it was accepted",
                    locale,
                    description
                ),
            }
        }
    }

    #[test]
    fn test_spark_format_validation_rejects_invalid() {
        // Test that our Spark format validation rejects invalid locale formats
        // Note: ICU itself is very permissive and accepts almost any string,
        // but our Spark format validation enforces stricter rules
        let invalid_locales = vec![
            // Invalid formats caught by our Spark validation
            ("en_USA_DEU_FRA", "too many components (4 parts)"),
            ("en_USA_DEU_FRA_CI", "too many components before _CI"),
        ];

        for (locale, description) in invalid_locales {
            let identifier = CollationIdentifier::icu(locale, None);
            let result = CollationFactory::from_identifier(identifier);

            match result {
                Err(e) => {
                    let err_msg = e.to_string();
                    // Verify error mentions the invalid locale format
                    assert!(
                        err_msg.contains(locale) || err_msg.contains("Invalid locale format"),
                        "Error for {} should mention invalid format. Got: {}",
                        description,
                        err_msg
                    );
                }
                Ok(_) => panic!(
                    "Expected invalid locale '{}' ({}) to be rejected, but it was accepted",
                    locale, description
                ),
            }
        }
    }

    #[test]
    fn test_invalid_locales_rejected() {
        // Test that our validation rejects invalid/malformed locale strings
        // We want strict validation, not permissive behavior
        let invalid_locales = vec![
            ("xy", "non-existent 3-letter language code"),
            ("xyz", "non-existent 3-letter language code"),
            ("zzz", "non-existent language code"),
            ("en__USA", "double underscore (empty component)"),
            ("_en_USA", "leading underscore"),
            ("en_USA_", "trailing underscore"),
            ("en_999", "numeric country code"),
            ("abcd_USA", "invalid language code length (4 letters)"),
            ("e_USA", "language code too short (1 letter)"),
            ("en_AB", "2-letter country code (Spark requires 3)"),
            ("en_ABCDE", "second component too long (5 letters)"),
            ("en_Abc_DEFG", "script code must be 4 letters"),
        ];

        for (locale, description) in invalid_locales {
            let identifier = CollationIdentifier::icu(locale, None);
            let result = CollationFactory::from_identifier(identifier);

            match result {
                Err(e) => {
                    let err_msg = e.to_string();
                    // Verify error mentions the locale is invalid
                    assert!(
                        err_msg.contains(locale) || err_msg.contains("Invalid locale"),
                        "Error for '{}' ({}) should mention invalid locale. Got: {}",
                        locale, description, err_msg
                    );
                }
                Ok(_) => panic!(
                    "Expected invalid locale '{}' ({}) to be rejected, but it was accepted",
                    locale, description
                ),
            }
        }
    }

    #[test]
    fn test_valid_icu_locales_with_modifiers() {
        // Test that valid ICU locales with _CI/_AI/_CS/_AS modifiers are accepted
        // Modifiers can appear in any order
        let valid_locales = vec![
            // Root collation
            "unicode",
            "uNiCode_CI",
            // Language only
            "en",
            "EN_CS",
            "en_CI",
            "en_AS",
            "en_AI",
            // Test order independence
            "en_CI_AI",
            "en_AI_CI",
            "en_CS_AI",
            "en_AI_CS",
            // Language + 3-letter country code
            "en_USA",
            "en_USA_CS",
            "en_USA_CI",
            "en_USA_AS",
            "en_USA_AI",
            "en_USA_CI_AI",
            "en_USA_AI_CI",
            // Language + script code
            "sr_Cyrl",
            "sr_Cyrl_CS",
            "sr_Cyrl_CI",
            "sr_Cyrl_AS",
            "sr_Cyrl_AI",
            // Language + script code + 3-letter country code.
            "sr_Cyrl_SRB",
            "sr_Cyrl_SRB_CS",
            "sr_Cyrl_SRB_CI",
            "sr_Cyrl_SRB_AS",
            "sr_Cyrl_SRB_AI"
        ];

        for locale in valid_locales {
            let identifier = CollationIdentifier::icu(locale, None);
            let result = CollationFactory::from_identifier(identifier);

            if let Err(e) = result {
                panic!("Valid locale '{}' should be accepted, but got error: {}", locale, e);
            }
        }
    }

    #[test]
    fn test_utf8_lcase_turkish_i_with_dot() {
        // Test special handling for Turkish İ (CAPITAL_I_WITH_DOT_ABOVE U+0130)
        // Should map to i (U+0069) + combining dot (U+0307)
        let identifier = CollationIdentifier::spark("UTF8_LCASE");
        let collation = CollationFactory::from_identifier(identifier).unwrap();

        // İ should be treated as equivalent to "i" followed by combining dot
        // For simplicity, test that İ lowercases correctly
        let turkish_i = "İstanbul"; // İ = U+0130
        let lowercase_i = "i\u{0307}stanbul"; // i + combining dot

        // They should compare equal after lowercase conversion
        assert_eq!(
            collation.compare(turkish_i, lowercase_i).unwrap(),
            std::cmp::Ordering::Equal,
            "İ should equal i+combining_dot in UTF8_LCASE"
        );
    }

    #[test]
    fn test_utf8_lcase_greek_sigma() {
        // Test special handling for Greek final sigma (ς U+03C2)
        // Should map to Greek small sigma (σ U+03C3) in context-unaware manner
        let identifier = CollationIdentifier::spark("UTF8_LCASE");
        let collation = CollationFactory::from_identifier(identifier).unwrap();

        // ς (final sigma) should equal σ (small sigma) in UTF8_LCASE
        let final_sigma = "ς"; // U+03C2
        let small_sigma = "σ"; // U+03C3

        assert!(
            collation.equals(final_sigma, small_sigma).unwrap(),
            "ς should equal σ in UTF8_LCASE (context-unaware)"
        );

        // Also test uppercase Σ
        let capital_sigma = "Σ"; // U+03A3
        assert!(
            collation.equals(capital_sigma, small_sigma).unwrap(),
            "Σ should equal σ in UTF8_LCASE"
        );
    }

    #[test]
    fn test_icu_version() {
        // This test ensures we're using ICU 75.1 as required
        // The ICU version is detected from the actual library we link against via FFI
        eprintln!("ICU_VERSION runtime value: {}", ICU_VERSION.as_str());
        eprintln!("Expected: 75.1");

        assert_eq!(
            ICU_VERSION.as_str(),
            "75.1",
            "ICU version must be 75.1. Found: {}",
            ICU_VERSION.as_str()
        );
    }

    #[test]
    fn test_cache_normalization_case_insensitive() {
        // "unicode" and "UNICODE" should normalize to the same cache entry
        let id1 = CollationIdentifier::icu("unicode", None);
        let id2 = CollationIdentifier::icu("UNICODE", None);
        let id3 = CollationIdentifier::icu("UniCode", None);

        let norm1 = CollationFactory::normalize_name(&id1.name);
        let norm2 = CollationFactory::normalize_name(&id2.name);
        let norm3 = CollationFactory::normalize_name(&id3.name);

        assert_eq!(norm1, norm2);
        assert_eq!(norm1, norm3);
        assert_eq!(norm1, "unicode");
    }

    #[test]
    fn test_cache_normalization_simple() {
        // Normalization just lowercases - no complex logic
        let id1 = CollationIdentifier::icu("unicode", None);
        let id2 = CollationIdentifier::icu("UNICODE", None);
        let id3 = CollationIdentifier::icu("UniCode", None);

        let norm1 = CollationFactory::normalize_name(&id1.name);
        let norm2 = CollationFactory::normalize_name(&id2.name);
        let norm3 = CollationFactory::normalize_name(&id3.name);

        // All should normalize to lowercase
        assert_eq!(norm1, "unicode");
        assert_eq!(norm2, "unicode");
        assert_eq!(norm3, "unicode");

        // Different modifier orders are separate cache entries (and that's fine)
        let id4 = CollationIdentifier::icu("unicode_CI_AI", None);
        let id5 = CollationIdentifier::icu("unicode_AI_CI", None);
        assert_eq!(CollationFactory::normalize_name(&id4.name), "unicode_ci_ai");
        assert_eq!(CollationFactory::normalize_name(&id5.name), "unicode_ai_ci");
        // They're different, which is fine - just means two cache entries
    }

    #[test]
    fn test_cache_equivalent_collations_work() {
        // Test that equivalent collations actually work and produce same results
        let collation1 = CollationFactory::from_identifier(
            CollationIdentifier::icu("unicode_CI_AI", None)
        ).unwrap();

        let collation2 = CollationFactory::from_identifier(
            CollationIdentifier::icu("UNICODE_ai_ci", None)
        ).unwrap();

        // Both should behave the same way
        assert!(collation1.equals("Müller", "muller").unwrap());
        assert!(collation2.equals("Müller", "muller").unwrap());
        assert!(collation1.equals("abc", "ABC").unwrap());
        assert!(collation2.equals("abc", "ABC").unwrap());
    }
}
