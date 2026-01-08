//! Collation support for string comparison operations.
//!
//! This module provides types for specifying collation identifiers that control how strings
//! are compared in expressions. Collations can specify case sensitivity, locale-specific
//! ordering rules, and other comparison semantics.
//!
//! ## Spark Compatibility
//!
//! Collation names follow the Spark collation semantics:
//! - Format: `language[_Script][_Country]`
//! - Language: 2-letter ISO 639-1 code (e.g., `en`, `fr`, `zh`)
//! - Script: 4-letter ISO 15924 code (optional, e.g., `Hant`, `Cyrl`)
//! - Country: 3-letter ISO 3166 code (e.g., `USA`, `CAN`, `MAC`)
//! - Case insensitive: `en_USA`, `EN_USA`, `en_usa` are equivalent
//!
//! Examples: `UNICODE`, `en_USA`, `fr_CAN`, `zh_Hant_MAC`, `sr_Cyrl_SRB`

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
    /// - ICU: "UNICODE", "en_USA", "de_DEU", "ja_JPN", etc.
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


    /// Creates a collation identifier with a custom provider.
    pub fn custom(provider: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(CollationProvider::Other(provider.into()), name, None)
    }

    /// Returns true if this is Spark's UTF8_BINARY collation (default binary comparison).
    pub fn is_spark_utf8_binary(&self) -> bool {
        matches!(self.provider, CollationProvider::Spark)
            && self.name.eq_ignore_ascii_case("UTF8_BINARY")
    }

    /// Returns true if this is Spark's UTF8_LCASE collation (case-insensitive).
    pub fn is_spark_utf8_lcase(&self) -> bool {
        matches!(self.provider, CollationProvider::Spark)
            && self.name.eq_ignore_ascii_case("UTF8_LCASE")
    }

    /// Returns true if this collation is supported by the kernel.
    ///
    /// - Spark UTF8_BINARY is always supported (standard binary comparison)
    /// - Spark UTF8_LCASE is supported (requires ICU for exact Spark compatibility)
    /// - ICU collations are supported if they are valid (proper locale format and available in ICU)
    ///
    /// This delegates to `CollationFactory::is_supported()` which performs full validation.
    pub fn is_supported(&self) -> bool {
        crate::collation_factory::CollationFactory::is_supported(self)
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
                "Collation '{}' is not supported. Supported: spark.UTF8_BINARY, spark.UTF8_LCASE, and valid ICU collations.",
                self
            );
            Err(crate::Error::unsupported(msg))
        }
    }

    /// Creates statsWithCollation lookup key: "icu.UNICODE.75.1" or "spark.UTF8_LCASE.75.1"
    ///
    /// For collations with explicit versions, returns the key to look up in statsWithCollation.
    /// For collations without version (version-agnostic), uses the kernel's ICU version.
    /// Returns an error if the collation is UTF8_BINARY (which doesn't use statsWithCollation).
    pub fn to_stats_key(&self) -> crate::DeltaResult<String> {
        // UTF8_BINARY doesn't use statsWithCollation
        if self.is_spark_utf8_binary() {
            let msg = format!(
                "Collation '{}' does not require statsWithCollation",
                self
            );
            return Err(crate::Error::generic(msg));
        }

        // Get version: use existing version, or kernel's ICU version if None (version-agnostic)
        let version = match &self.version {
            Some(v) => v.clone(),
            None => {
                // Version-agnostic: use kernel's ICU version
                crate::collation_factory::ICU_VERSION.to_string()
            }
        };

        Ok(format!("{}.{}.{}", self.provider, self.name, version))
    }

    /// Returns true if this collation needs statsWithCollation (anything except UTF8_BINARY)
    /// UTF8_BINARY uses regular minValues/maxValues stats
    pub fn requires_stats_with_collation(&self) -> bool {
        !self.is_spark_utf8_binary()
    }

    /// Returns true if this collation's version is compatible with the kernel's ICU version.
    ///
    /// Version compatibility rules:
    /// - UTF8_BINARY: always compatible (no version)
    /// - Version specified: must match kernel's ICU version exactly
    /// - Version None (version-agnostic): always compatible (uses kernel's version)
    ///
    /// This is used to determine if stats or comparisons can be safely used with this collation.
    pub fn is_version_compatible(&self) -> bool {
        // UTF8_BINARY doesn't need version validation
        if self.is_spark_utf8_binary() {
            return true;
        }

        // If version is specified, it must match the kernel's ICU version
        if let Some(ref version) = self.version {
            return version.as_str() == &*crate::collation_factory::ICU_VERSION;
        }

        // Version-agnostic (None) is always compatible
        true
    }

}

impl Display for CollationIdentifier {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.provider, self.name)?;
        if let Some(ref version) = self.version {
            write!(f, ".{}", version)?;
        }
        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collation_identifier_display() {
        let collation = CollationIdentifier::spark("UTF8_BINARY");
        assert_eq!(collation.to_string(), "spark.UTF8_BINARY");

        let collation = CollationIdentifier::icu("de_DEU".to_string(), Some("75.1".to_string()));
        assert_eq!(collation.to_string(), "icu.de_DEU.75.1");

        let collation = CollationIdentifier::icu("UNICODE".to_string(), None);
        assert_eq!(collation.to_string(), "icu.UNICODE");

        let collation = CollationIdentifier::custom("custom_provider", "custom_collation");
        assert_eq!(collation.to_string(), "custom_provider.custom_collation");
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

        // Utf8_Binary (mixed case) is also supported
        let collation = CollationIdentifier::spark("Utf8_Binary");
        assert!(collation.is_spark_utf8_binary());
        assert!(collation.is_supported());

        // UTF8_LCASE is not UTF8_BINARY but is supported
        let collation = CollationIdentifier::spark("UTF8_LCASE");
        assert!(!collation.is_spark_utf8_binary());
        assert!(collation.is_spark_utf8_lcase());
        assert!(collation.is_supported());

        // utf8_lcase (lowercase) is also supported
        let collation = CollationIdentifier::spark("utf8_lcase");
        assert!(collation.is_spark_utf8_lcase());
        assert!(collation.is_supported());

        // Utf8_Lcase (mixed case) is also supported
        let collation = CollationIdentifier::spark("Utf8_Lcase");
        assert!(collation.is_spark_utf8_lcase());
        assert!(collation.is_supported());

        // ICU collations are not UTF8_BINARY
        let collation = CollationIdentifier::icu("en".to_string(), None);
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
        let collation = CollationIdentifier::icu("de".to_string(), Some("75.1".to_string()));
        assert!(collation.validate_support().is_ok());
    }

    #[test]
    fn test_icu_is_supported() {
        // With icu-collation feature, valid ICU collations should be supported
        let collation = CollationIdentifier::icu("en".to_string(), Some("75.1".to_string()));
        assert!(collation.is_supported(), "Valid ICU collations should be supported");

        // Invalid ICU collation should not be supported
        let collation = CollationIdentifier::icu("invalid_xyz".to_string(), None);
        assert!(!collation.is_supported(), "Invalid ICU collations should not be supported");
    }

    #[test]
    fn test_spark_locale_naming() {
        // Test that locale names use Spark format with _CI/_AI suffixes
        let collation = CollationIdentifier::icu("fr_CAN", None);
        assert_eq!(collation.name, "fr_CAN");

        let collation = CollationIdentifier::icu("zh_Hant_MAC_CI", None);
        assert_eq!(collation.name, "zh_Hant_MAC_CI");

        // Multiple modifiers
        let collation = CollationIdentifier::icu("sr_Cyrl_SRB_CI_AI", None);
        assert_eq!(collation.name, "sr_Cyrl_SRB_CI_AI");

        // Display format
        assert_eq!(collation.to_string(), "icu.sr_Cyrl_SRB_CI_AI");
    }

    #[test]
    fn test_collation_key_format() {
        use crate::collation_factory::ICU_VERSION;

        // Test ICU collation with explicit version
        let collation = CollationIdentifier {
            provider: CollationProvider::Icu,
            name: "en_US".to_string(),
            version: Some("75.1".to_string()),
        };
        assert_eq!(collation.to_stats_key().unwrap(), "icu.en_US.75.1");

        // Test UTF8_BINARY (should error - doesn't use statsWithCollation)
        let binary = CollationIdentifier {
            provider: CollationProvider::Spark,
            name: "UTF8_BINARY".to_string(),
            version: None,
        };
        assert!(binary.to_stats_key().is_err());

        // Test UTF8_LCASE without version (should use kernel's ICU version)
        let lcase = CollationIdentifier {
            provider: CollationProvider::Spark,
            name: "UTF8_LCASE".to_string(),
            version: None,
        };
        let lcase_key = lcase.to_stats_key().unwrap();
        assert_eq!(lcase_key, format!("spark.UTF8_LCASE.{}", *ICU_VERSION));

        // Test UTF8_LCASE with explicit version
        let lcase_explicit = CollationIdentifier {
            provider: CollationProvider::Spark,
            name: "UTF8_LCASE".to_string(),
            version: Some("75.1".to_string()),
        };
        assert_eq!(
            lcase_explicit.to_stats_key().unwrap(),
            "spark.UTF8_LCASE.75.1"
        );
    }

    #[test]
    fn test_requires_stats_with_collation() {
        // UTF8_BINARY doesn't require statsWithCollation
        let binary = CollationIdentifier {
            provider: CollationProvider::Spark,
            name: "UTF8_BINARY".to_string(),
            version: None,
        };
        assert!(!binary.requires_stats_with_collation());

        // ICU collations require statsWithCollation
        let icu = CollationIdentifier {
            provider: CollationProvider::Icu,
            name: "en_US".to_string(),
            version: Some("75.1".to_string()),
        };
        assert!(icu.requires_stats_with_collation());

        // UTF8_LCASE requires statsWithCollation
        let lcase = CollationIdentifier {
            provider: CollationProvider::Spark,
            name: "UTF8_LCASE".to_string(),
            version: None,
        };
        assert!(lcase.requires_stats_with_collation());
    }

    #[test]
    fn test_collation_version_compatibility() {
        use crate::collation_factory::ICU_VERSION;

        // Matching version should be compatible
        let matching = CollationIdentifier {
            provider: CollationProvider::Icu,
            name: "en_US".to_string(),
            version: Some(ICU_VERSION.to_string()),
        };
        assert!(matching.is_version_compatible());

        // Mismatched version should not be compatible
        let wrong_version = if *ICU_VERSION == "75.1" {
            "76.0"
        } else {
            "75.1"
        };
        let mismatched = CollationIdentifier {
            provider: CollationProvider::Icu,
            name: "en_US".to_string(),
            version: Some(wrong_version.to_string()),
        };
        assert!(!mismatched.is_version_compatible());

        // UTF8_BINARY is always compatible
        let binary = CollationIdentifier {
            provider: CollationProvider::Spark,
            name: "UTF8_BINARY".to_string(),
            version: None,
        };
        assert!(binary.is_version_compatible());

        // UTF8_LCASE without version uses kernel's ICU version (compatible)
        let lcase = CollationIdentifier {
            provider: CollationProvider::Spark,
            name: "UTF8_LCASE".to_string(),
            version: None,
        };
        assert!(lcase.is_version_compatible());

        // UTF8_LCASE with wrong version is not compatible
        let lcase_wrong = CollationIdentifier {
            provider: CollationProvider::Spark,
            name: "UTF8_LCASE".to_string(),
            version: Some(wrong_version.to_string()),
        };
        assert!(!lcase_wrong.is_version_compatible());
    }

}
