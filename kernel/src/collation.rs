//! Collation support for string comparison operations.
//!
//! This module provides types for specifying collation identifiers that control how strings
//! are compared in expressions. Collations can specify case sensitivity, locale-specific
//! ordering rules, and other comparison semantics.

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
/// - `provider`: The collation implementation provider (e.g., Spark, ICU)
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

    /// Creates a collation identifier with a custom provider.
    pub fn custom(provider: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(CollationProvider::Other(provider.into()), name, None)
    }

    /// Returns true if this is Spark's UTF8_BINARY collation (default binary comparison).
    pub fn is_spark_utf8_binary(&self) -> bool {
        matches!(self.provider, CollationProvider::Spark)
            && (self.name == "UTF8_BINARY" || self.name == "utf8_binary")
    }

    /// Returns true if this collation is supported by the kernel.
    ///
    /// Currently, only Spark's UTF8_BINARY collation is supported, which is equivalent
    /// to standard binary (byte-wise) comparison.
    pub fn is_supported(&self) -> bool {
        self.is_spark_utf8_binary()
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
            Err(crate::Error::unsupported(format!(
                "Collation '{}' is not supported. Only 'spark:UTF8_BINARY' is currently supported.",
                self
            )))
        }
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

        // Other Spark collations are not UTF8_BINARY
        let collation = CollationIdentifier::spark("UTF8_LCASE");
        assert!(!collation.is_spark_utf8_binary());
        assert!(!collation.is_supported());

        // ICU collations are not UTF8_BINARY
        let collation = CollationIdentifier::icu("en_US".to_string(), None);
        assert!(!collation.is_spark_utf8_binary());
        assert!(!collation.is_supported());
    }

    #[test]
    fn test_validate_support() {
        // UTF8_BINARY should validate successfully
        let collation = CollationIdentifier::spark("UTF8_BINARY");
        assert!(collation.validate_support().is_ok());

        // UTF8_LCASE should error
        let collation = CollationIdentifier::spark("UTF8_LCASE");
        let result = collation.validate_support();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("not supported"));
        assert!(err.to_string().contains("UTF8_LCASE"));

        // ICU should error
        let collation = CollationIdentifier::icu("de_DE".to_string(), Some("75.1".to_string()));
        let result = collation.validate_support();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("not supported"));
    }
}
