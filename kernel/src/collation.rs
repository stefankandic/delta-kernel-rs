//! Collation support for string comparison operations.
//!
//! This module provides types for specifying collation identifiers that control how strings
//! are compared in expressions. Collations can specify case sensitivity, locale-specific
//! ordering rules, and other comparison semantics.
//!
//! ## Spark/Databricks Compatibility
//!
//! Collation names follow the Spark/Databricks collation semantics:
//! - Format: `language[_Script][_Country]`
//! - Language: 2-letter ISO 639-1 code (e.g., `en`, `fr`, `zh`)
//! - Script: 4-letter ISO 15924 code (optional, e.g., `Hant`, `Cyrl`)
//! - Country: 2 or 3-letter ISO 3166 code (e.g., `US`, `CAN`, `MAC`)
//! - Case insensitive: `en_US`, `EN_US`, `en_us` are equivalent
//!
//! Examples: `en`, `en_US`, `fr_CAN`, `zh_Hant_MAC`, `sr_Cyrl_RS`

use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};

/// Identifies a collation provider that implements string comparison logic.
///
/// Different providers may have different collation implementations and version schemes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollationProvider {
    /// Apache Spark collation implementation
    Spark,
    /// ICU (International Components for Unicode) collation library
    Icu,
    /// Custom or other collation provider
    Other(String),
}

impl Display for CollationProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            CollationProvider::Spark => write!(f, "spark"),
            CollationProvider::Icu => write!(f, "icu"),
            CollationProvider::Other(name) => write!(f, "{}", name),
        }
    }
}

/// A complete collation identifier specifying how strings should be compared.
///
/// # Structure
///
/// - `provider`: The collation implementation provider (e.g., spark, icu)
/// - `name`: The collation name within that provider (e.g., "UTF8_BINARY", "de_DE")
/// - `version`: Optional version of the collation implementation (e.g., "75.1" for ICU)
///
/// # Examples
///
/// ```ignore
/// // Spark's UTF-8 binary collation (case-sensitive)
/// CollationIdentifier {
///     provider: CollationProvider::Spark,
///     name: "UTF8_BINARY".to_string(),
///     version: None,
/// }
///
/// // ICU German locale collation with specific version
/// CollationIdentifier {
///     provider: CollationProvider::Icu,
///     name: "de_DE".to_string(),
///     version: Some("75.1".to_string()),
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CollationIdentifier {
    /// The collation provider (e.g., Spark, ICU)
    pub provider: CollationProvider,

    /// The collation name within the provider's namespace.
    ///
    /// Examples:
    /// - Spark: "UTF8_BINARY", "UTF8_LCASE", "UNICODE", "UNICODE_CI"
    /// - ICU: "en_US", "de_DE", "ja_JP", etc.
    pub name: String,

    /// Optional version identifier for the collation implementation.
    ///
    /// This is particularly important for ICU collations where different versions
    /// may produce different sort orders. For example, ICU version "75.1" or "76.0".
    ///
    /// If `None`, the implementation should use a default or system version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl CollationIdentifier {
    /// Creates a new collation identifier.
    pub fn new(
        provider: CollationProvider,
        name: impl Into<String>,
        version: Option<String>,
    ) -> Self {
        Self {
            provider,
            name: name.into(),
            version,
        }
    }

    /// Creates a Spark collation identifier.
    pub fn spark(name: impl Into<String>) -> Self {
        Self::new(CollationProvider::Spark, name, None)
    }

    /// Creates an ICU collation identifier with an optional version.
    pub fn icu(name: impl Into<String>, version: Option<String>) -> Self {
        Self::new(CollationProvider::Icu, name, version)
    }

    /// Creates a builder for configuring an ICU collation with options.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let collation = CollationIdentifier::icu_builder("en_US")
    ///     .case_sensitive(false)
    ///     .accent_sensitive(false)
    ///     .build();
    /// // Result: "icu:en_US_CI_AI"
    /// ```
    pub fn icu_builder(locale: impl Into<String>) -> IcuCollationBuilder {
        IcuCollationBuilder::new(locale)
    }

    /// Creates a collation identifier with a custom provider.
    pub fn custom(provider: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(CollationProvider::Other(provider.into()), name, None)
    }

    /// Returns true if this is Spark's UTF8_BINARY collation (default binary comparison).
    pub fn is_spark_utf8_binary(&self) -> bool {
        matches!(self.provider, CollationProvider::Spark)
            && (self.name == "UTF8_BINARY" || self.name == "utf8_binary")
    }

    /// Returns true if this is Spark's UTF8_LCASE collation (case-insensitive).
    pub fn is_spark_utf8_lcase(&self) -> bool {
        matches!(self.provider, CollationProvider::Spark)
            && (self.name == "UTF8_LCASE" || self.name == "utf8_lcase")
    }

    /// Returns true if this collation is supported by the kernel.
    ///
    /// - Spark UTF8_BINARY is always supported (standard binary comparison)
    /// - Spark UTF8_LCASE is supported (requires ICU for exact Spark compatibility)
    /// - ICU collations are supported
    pub fn is_supported(&self) -> bool {
        // UTF8_BINARY is always supported
        if self.is_spark_utf8_binary() {
            return true;
        }

        // UTF8_LCASE and ICU collations are supported
        if self.is_spark_utf8_lcase() || matches!(self.provider, CollationProvider::Icu) {
            return true;
        }

        false
    }

    /// Validates that this collation is supported, returning an error if not.
    ///
    /// # Errors
    ///
    /// Returns an error if the collation is not supported by the kernel.
    pub fn validate_support(&self) -> crate::DeltaResult<()> {
        if self.is_supported() {
            Ok(())
        } else {
            let msg = format!(
                "Collation '{}' is not supported. Supported: spark:UTF8_BINARY, spark:UTF8_LCASE, and ICU collations.",
                self
            );
            Err(crate::Error::unsupported(msg))
        }
    }

    /// Converts Spark/Databricks collation name to ICU locale string with keywords.
    ///
    /// Converts from Spark format (e.g., `de_CI_AI`) to ICU format (e.g., `de@colStrength=primary`)
    /// for use with ICU collator APIs.
    ///
    /// # Spark Modifiers (case-insensitive)
    ///
    /// - _CI: Case insensitive
    /// - _CS: Case sensitive (explicit, default)
    /// - _AI: Accent insensitive
    /// - _AS: Accent sensitive (explicit, default)
    ///
    /// # ICU Mapping
    ///
    /// - CI + AI → PRIMARY strength (ignores case and accents)
    /// - CI + AS → SECONDARY strength (ignores case, considers accents)
    /// - CS + AI → PRIMARY strength + case level ON (considers case, ignores accents)
    /// - CS + AS → TERTIARY strength (considers case and accents, default)
    ///
    /// # Returns
    ///
    /// ICU locale string suitable for creating a UCollator
    pub fn to_icu_locale_string(&self) -> String {
        if !matches!(self.provider, CollationProvider::Icu) {
            return self.name.clone();
        }

        let upper = self.name.to_uppercase();
        let mut locale = self.name.clone();

        // Parse sensitivity modifiers (case-insensitive, order independent)
        // Default: case sensitive, accent sensitive (if no modifier specified)
        let mut case_sensitive = true;
        let mut accent_sensitive = true;

        let has_ci = upper.contains("_CI");
        let has_cs = upper.contains("_CS");
        let has_ai = upper.contains("_AI");
        let has_as = upper.contains("_AS");

        // Determine case sensitivity: only change from default if explicitly specified
        if has_ci {
            case_sensitive = false;
        } else if has_cs {
            case_sensitive = true;
        }
        // If neither _CI nor _CS specified, keep default (true)

        // Determine accent sensitivity: only change from default if explicitly specified
        if has_ai {
            accent_sensitive = false;
        } else if has_as {
            accent_sensitive = true;
        }
        // If neither _AI nor _AS specified, keep default (true)

        // Strip all possible suffix combinations (case-insensitive)
        // Order matters: check longer suffixes first
        if upper.ends_with("_CI_AI") || upper.ends_with("_CS_AS") ||
           upper.ends_with("_AI_CI") || upper.ends_with("_AS_CS") ||
           upper.ends_with("_CI_AS") || upper.ends_with("_CS_AI") ||
           upper.ends_with("_AI_CS") || upper.ends_with("_AS_CI") {
            locale.truncate(locale.len() - 6);
        } else if upper.ends_with("_CI") || upper.ends_with("_AI") ||
                  upper.ends_with("_CS") || upper.ends_with("_AS") {
            locale.truncate(locale.len() - 3);
        }

        // Map to ICU strength and options
        // Note: rust_icu uses older @ parameter format, not Unicode locale keywords
        // Spark/Databricks uses programmatic LocaleBuilder API with setUnicodeLocaleKeyword
        // but rust_icu parses string format, so we use colStrength/colCaseLevel
        //
        // Reference: https://unicode-org.github.io/icu/userguide/collation/concepts.html#comparison-levels
        match (case_sensitive, accent_sensitive) {
            (false, false) => {
                // CI + AI: primary strength (ignores case and accents)
                locale.push_str("@colStrength=primary");
            }
            (false, true) => {
                // CI + AS: secondary strength (ignores case, considers accents)
                locale.push_str("@colStrength=secondary");
            }
            (true, false) => {
                // CS + AI: primary strength + case level (considers case, ignores accents)
                // Examples: "unicode_AI", "unicode_CS_AI", "unicode_AI_CS"
                // This matches Spark/Databricks: ks=level1;kc=true
                locale.push_str("@colStrength=primary;colCaseLevel=yes");
            }
            (true, true) => {
                // CS + AS: tertiary strength (considers case and accents, this is the default)
                // No need to add anything, tertiary is default
            }
        }

        locale
    }
}

impl Display for CollationIdentifier {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.provider, self.name)?;
        if let Some(ref version) = self.version {
            write!(f, ":{}", version)?;
        }
        Ok(())
    }
}

/// Builder for creating ICU collations with case and accent sensitivity options.
///
/// ICU collations support fine-grained control over string comparison behavior through
/// collation keywords appended to the locale string.
///
/// # Example
///
/// ```ignore
/// use delta_kernel::collation::CollationIdentifier;
///
/// // Case-insensitive, accent-sensitive English collation
/// let collation = CollationIdentifier::icu_builder("en_US")
///     .case_sensitive(false)
///     .accent_sensitive(true)
///     .version("75.1")
///     .build();
///
/// assert_eq!(collation.name, "en_US@colCaseLevel=no;colStrength=tertiary");
/// ```
#[derive(Debug, Clone)]
pub struct IcuCollationBuilder {
    locale: String,
    case_sensitive: Option<bool>,
    accent_sensitive: Option<bool>,
    version: Option<String>,
}

impl IcuCollationBuilder {
    /// Creates a new builder for an ICU collation with the specified locale.
    ///
    /// # Arguments
    ///
    /// * `locale` - The ICU locale identifier (e.g., "en_US", "de_DE", "ja_JP")
    pub fn new(locale: impl Into<String>) -> Self {
        Self {
            locale: locale.into(),
            case_sensitive: None,
            accent_sensitive: None,
            version: None,
        }
    }

    /// Sets case sensitivity for the collation.
    ///
    /// - `true`: Case-sensitive comparison ("a" != "A")
    /// - `false`: Case-insensitive comparison ("a" == "A")
    ///
    /// Default: `true` (case-sensitive)
    pub fn case_sensitive(mut self, sensitive: bool) -> Self {
        self.case_sensitive = Some(sensitive);
        self
    }

    /// Sets accent sensitivity for the collation.
    ///
    /// - `true`: Accent-sensitive comparison ("a" != "á")
    /// - `false`: Accent-insensitive comparison ("a" == "á")
    ///
    /// Default: `true` (accent-sensitive)
    pub fn accent_sensitive(mut self, sensitive: bool) -> Self {
        self.accent_sensitive = Some(sensitive);
        self
    }

    /// Sets the ICU version metadata for this collation.
    ///
    /// This is optional metadata for tracking/compatibility purposes.
    ///
    /// # Arguments
    ///
    /// * `version` - ICU version string (e.g., "75.1")
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Builds the collation identifier with the configured options.
    ///
    /// The collation name uses Spark/Databricks format with `_CI` and `_AI` suffixes:
    /// - `_CI`: Case insensitive
    /// - `_AI`: Accent insensitive
    ///
    /// Examples: `en_US`, `de_CI`, `fr_CAN_AI`, `unicode_CI_AI`
    pub fn build(self) -> CollationIdentifier {
        let mut name = self.locale;

        // Add Spark/Databricks format suffixes
        let case_insensitive = matches!(self.case_sensitive, Some(false));
        let accent_insensitive = matches!(self.accent_sensitive, Some(false));

        if case_insensitive {
            name.push_str("_CI");
        }
        if accent_insensitive {
            name.push_str("_AI");
        }

        CollationIdentifier {
            provider: CollationProvider::Icu,
            name,
            version: self.version,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collation_identifier_display() {
        let collation = CollationIdentifier::spark("UTF8_BINARY");
        assert_eq!(collation.to_string(), "spark:UTF8_BINARY");

        let collation = CollationIdentifier::icu("de_DE".to_string(), Some("75.1".to_string()));
        assert_eq!(collation.to_string(), "icu:de_DE:75.1");

        let collation = CollationIdentifier::icu("en_US".to_string(), None);
        assert_eq!(collation.to_string(), "icu:en_US");

        let collation = CollationIdentifier::custom("custom_provider", "custom_collation");
        assert_eq!(collation.to_string(), "custom_provider:custom_collation");
    }

    #[test]
    fn test_collation_identifier_serde() {
        let collation = CollationIdentifier::icu("de_DE".to_string(), Some("75.1".to_string()));
        let json = serde_json::to_string(&collation).unwrap();
        let deserialized: CollationIdentifier = serde_json::from_str(&json).unwrap();
        assert_eq!(collation, deserialized);

        // Test with no version
        let collation = CollationIdentifier::spark("UTF8_LCASE");
        let json = serde_json::to_string(&collation).unwrap();
        let deserialized: CollationIdentifier = serde_json::from_str(&json).unwrap();
        assert_eq!(collation, deserialized);

        // Ensure version: null is omitted in JSON
        assert!(!json.contains("version"));
    }

    #[test]
    fn test_provider_display() {
        assert_eq!(CollationProvider::Spark.to_string(), "spark");
        assert_eq!(CollationProvider::Icu.to_string(), "icu");
        assert_eq!(
            CollationProvider::Other("custom".to_string()).to_string(),
            "custom"
        );
    }

    #[test]
    fn test_is_spark_utf8_binary() {
        // UTF8_BINARY (uppercase) is supported
        let collation = CollationIdentifier::spark("UTF8_BINARY");
        assert!(collation.is_spark_utf8_binary());
        assert!(collation.is_supported());

        // utf8_binary (lowercase) is also supported
        let collation = CollationIdentifier::spark("utf8_binary");
        assert!(collation.is_spark_utf8_binary());
        assert!(collation.is_supported());

        // UTF8_LCASE is not UTF8_BINARY but is supported
        let collation = CollationIdentifier::spark("UTF8_LCASE");
        assert!(!collation.is_spark_utf8_binary());
        assert!(collation.is_spark_utf8_lcase());
        assert!(collation.is_supported());

        // ICU collations are not UTF8_BINARY
        let collation = CollationIdentifier::icu("en_US".to_string(), None);
        assert!(!collation.is_spark_utf8_binary());
        // ICU collations are supported
        assert!(collation.is_supported());
    }

    #[test]
    fn test_validate_support() {
        // UTF8_BINARY should validate successfully
        let collation = CollationIdentifier::spark("UTF8_BINARY");
        assert!(collation.validate_support().is_ok());

        // UTF8_LCASE should validate successfully
        let collation = CollationIdentifier::spark("UTF8_LCASE");
        assert!(collation.validate_support().is_ok());

        // Unsupported Spark collation should error
        let collation = CollationIdentifier::spark("UTF8_UNSUPPORTED");
        let result = collation.validate_support();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("not supported"));

        // ICU collations should be supported
        let collation = CollationIdentifier::icu("de_DE".to_string(), Some("75.1".to_string()));
        assert!(collation.validate_support().is_ok());
    }

    #[test]
    fn test_icu_version_available() {
        use rust_icu_ucol::UCollator;

        // Verify we can create a collator (which means ICU is working)
        // The actual version is checked by build.rs
        let collator = UCollator::try_from("en_US");
        assert!(collator.is_ok(), "ICU collator should be available");

        // Note: ICU 75.1 is required by build.rs for the default engine.
        // If this test passes, the correct version is installed.
    }

    #[test]
    fn test_icu_collator_creation() {
        use rust_icu_ucol::UCollator;

        // Test creating collators for different locales
        let collator = UCollator::try_from("en_US");
        assert!(collator.is_ok(), "Failed to create en_US collator");

        let collator = UCollator::try_from("de_DE");
        assert!(collator.is_ok(), "Failed to create de_DE collator");

        let collator = UCollator::try_from("ja_JP");
        assert!(collator.is_ok(), "Failed to create ja_JP collator");
    }

    #[test]
    fn test_icu_is_supported() {
        // With icu-collation feature, ICU collations should be supported
        let collation = CollationIdentifier::icu("en_US".to_string(), Some("75.1".to_string()));
        assert!(collation.is_supported(), "ICU collations should be supported with icu-collation feature");
    }

    #[test]
    fn test_icu_builder_basic() {
        // Basic locale without options
        let collation = CollationIdentifier::icu_builder("en_US").build();
        assert_eq!(collation.provider, CollationProvider::Icu);
        assert_eq!(collation.name, "en_US");
        assert_eq!(collation.version, None);
    }

    #[test]
    fn test_icu_builder_case_insensitive() {
        // Case-insensitive collation
        let collation = CollationIdentifier::icu_builder("en_US")
            .case_sensitive(false)
            .build();
        assert_eq!(collation.name, "en_US_CI");
    }

    #[test]
    fn test_icu_builder_accent_insensitive() {
        // Accent-insensitive collation
        let collation = CollationIdentifier::icu_builder("fr_FR")
            .accent_sensitive(false)
            .build();
        assert_eq!(collation.name, "fr_FR_AI");
    }

    #[test]
    fn test_icu_builder_case_and_accent_insensitive() {
        // Both case and accent insensitive (Spark/Databricks format)
        let collation = CollationIdentifier::icu_builder("de")
            .case_sensitive(false)
            .accent_sensitive(false)
            .build();
        assert_eq!(collation.name, "de_CI_AI");
    }

    #[test]
    fn test_icu_builder_with_version() {
        // Builder can optionally set version metadata
        let collation = CollationIdentifier::icu_builder("ja_JP")
            .case_sensitive(false)
            .version("75.1")
            .build();
        assert_eq!(collation.name, "ja_JP_CI");
        assert_eq!(collation.version, Some("75.1".to_string()));

        // Without version
        let collation = CollationIdentifier::icu_builder("ja_JP")
            .case_sensitive(false)
            .build();
        assert_eq!(collation.name, "ja_JP_CI");
        assert_eq!(collation.version, None);
    }

    #[test]
    fn test_icu_builder_explicit_sensitive() {
        // Explicitly set to sensitive (default behavior) - no suffixes added
        let collation = CollationIdentifier::icu_builder("en_US")
            .case_sensitive(true)
            .accent_sensitive(true)
            .build();
        assert_eq!(collation.name, "en_US");
    }

    #[test]
    fn test_icu_builder_display() {
        let collation = CollationIdentifier::icu_builder("en_US")
            .case_sensitive(false)
            .accent_sensitive(false)
            .build();
        assert_eq!(collation.to_string(), "icu:en_US_CI_AI");
    }

    #[test]
    fn test_unicode_root_collation() {
        // Root collation should be called "unicode" not "und"
        let unicode = CollationIdentifier::icu_builder("unicode").build();
        assert_eq!(unicode.name, "unicode");

        let unicode_ci = CollationIdentifier::icu_builder("unicode")
            .case_sensitive(false)
            .build();
        assert_eq!(unicode_ci.name, "unicode_CI");

        let unicode_ai = CollationIdentifier::icu_builder("unicode")
            .accent_sensitive(false)
            .build();
        assert_eq!(unicode_ai.name, "unicode_AI");

        let unicode_ci_ai = CollationIdentifier::icu_builder("unicode")
            .case_sensitive(false)
            .accent_sensitive(false)
            .build();
        assert_eq!(unicode_ci_ai.name, "unicode_CI_AI");
    }

    #[test]
    fn test_icu_builder_creates_valid_collator() {
        use rust_icu_ucol::UCollator;

        // Test that the builder creates valid Spark format that converts to ICU
        let collation = CollationIdentifier::icu_builder("en_US")
            .case_sensitive(false)
            .build();

        assert_eq!(collation.name, "en_US_CI");

        // Convert to ICU format for collator creation
        let icu_locale = collation.to_icu_locale_string();
        assert_eq!(icu_locale, "en_US@colStrength=secondary");

        // This should successfully create a collator
        let collator = UCollator::try_from(icu_locale.as_str());
        assert!(collator.is_ok(), "Converted ICU locale string should be valid");
    }

    #[test]
    fn test_to_icu_locale_string_conversion() {
        // Test Spark format to ICU format conversion
        // rust_icu uses @ parameter format (colStrength, colCaseLevel)

        // Base locale (no modifiers)
        let collation = CollationIdentifier::icu_builder("en_US").build();
        assert_eq!(collation.to_icu_locale_string(), "en_US");

        // Case insensitive
        let collation = CollationIdentifier::icu_builder("en_US")
            .case_sensitive(false)
            .build();
        assert_eq!(collation.name, "en_US_CI");
        assert_eq!(collation.to_icu_locale_string(), "en_US@colStrength=secondary");

        // Accent insensitive (CS+AI: colStrength=primary;colCaseLevel=yes)
        let collation = CollationIdentifier::icu_builder("de")
            .accent_sensitive(false)
            .build();
        assert_eq!(collation.name, "de_AI");
        assert_eq!(collation.to_icu_locale_string(), "de@colStrength=primary;colCaseLevel=yes");

        // Both case and accent insensitive
        let collation = CollationIdentifier::icu_builder("de")
            .case_sensitive(false)
            .accent_sensitive(false)
            .build();
        assert_eq!(collation.name, "de_CI_AI");
        assert_eq!(collation.to_icu_locale_string(), "de@colStrength=primary");

        // Unicode root collation with case insensitive
        let collation = CollationIdentifier::icu_builder("unicode")
            .case_sensitive(false)
            .build();
        assert_eq!(collation.name, "unicode_CI");
        assert_eq!(collation.to_icu_locale_string(), "unicode@colStrength=secondary");
    }

    #[test]
    fn test_spark_databricks_locale_formats() {
        use rust_icu_ucol::UCollator;

        // Test Spark/Databricks locale format compatibility

        // Language only
        let collation = CollationIdentifier::icu_builder("en").build();
        assert!(UCollator::try_from(collation.name.as_str()).is_ok());

        // Language + 2-letter country
        let collation = CollationIdentifier::icu_builder("en_US").build();
        assert!(UCollator::try_from(collation.name.as_str()).is_ok());

        // Language + 3-letter country (Spark/Databricks format)
        let collation = CollationIdentifier::icu_builder("fr_CAN").build();
        assert!(UCollator::try_from(collation.name.as_str()).is_ok());

        // Language + Script + Country (3-letter)
        let collation = CollationIdentifier::icu_builder("zh_Hant_MAC").build();
        assert!(UCollator::try_from(collation.name.as_str()).is_ok());

        // Language + Script + Country (2-letter)
        let collation = CollationIdentifier::icu_builder("sr_Cyrl_RS").build();
        assert!(UCollator::try_from(collation.name.as_str()).is_ok());

        // Root/undefined collation
        let collation = CollationIdentifier::icu_builder("und").build();
        assert!(UCollator::try_from(collation.name.as_str()).is_ok());
    }

    #[test]
    fn test_locale_case_insensitivity() {
        use rust_icu_ucol::UCollator;

        // Locale identifiers should be case-insensitive
        // ICU normalizes these internally
        let upper = CollationIdentifier::icu_builder("EN_US").build();
        let lower = CollationIdentifier::icu_builder("en_us").build();
        let mixed = CollationIdentifier::icu_builder("en_US").build();

        assert!(UCollator::try_from(upper.name.as_str()).is_ok());
        assert!(UCollator::try_from(lower.name.as_str()).is_ok());
        assert!(UCollator::try_from(mixed.name.as_str()).is_ok());
    }

    #[test]
    fn test_spark_databricks_locale_naming() {
        // Test that locale names use Spark/Databricks format with _CI/_AI suffixes
        let collation = CollationIdentifier::icu_builder("fr_CAN").build();
        assert_eq!(collation.name, "fr_CAN");

        let collation = CollationIdentifier::icu_builder("zh_Hant_MAC")
            .case_sensitive(false)
            .build();
        assert_eq!(collation.name, "zh_Hant_MAC_CI");

        // Multiple modifiers
        let collation = CollationIdentifier::icu_builder("sr_Cyrl_RS")
            .case_sensitive(false)
            .accent_sensitive(false)
            .build();
        assert_eq!(collation.name, "sr_Cyrl_RS_CI_AI");

        // Display format
        assert_eq!(collation.to_string(), "icu:sr_Cyrl_RS_CI_AI");
    }
}
