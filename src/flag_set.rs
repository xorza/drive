use crate::error::ConfigError;

/// Feature flags, and for a test lane the targets it selects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FlagSet {
    pub(crate) features: Vec<String>,
    pub(crate) targets: Vec<String>,
    /// For a set of every feature but some, those left out, which its label names in place of
    /// the long list.
    pub(crate) but: Vec<String>,
}

/// Whether a set may select targets: a test lane may, every other lane may not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Targets {
    Allowed,
    Refused,
}

impl FlagSet {
    pub(crate) fn all_features() -> Self {
        Self {
            features: vec!["--all-features".to_owned()],
            ..Self::default()
        }
    }

    pub(crate) fn parse(
        text: &str,
        key: &'static str,
        targets: Targets,
    ) -> Result<Self, ConfigError> {
        let mut set = Self::default();
        let mut tokens = text.split_whitespace();
        while let Some(token) = tokens.next() {
            let selects = targets == Targets::Allowed;
            if matches!(token, "--all-features" | "--no-default-features")
                || token.starts_with("--features=")
            {
                set.features.push(token.to_owned());
            } else if token == "--features" {
                let value = tokens
                    .next()
                    .ok_or(ConfigError::FeaturesNeedsValue { key })?;
                set.features.extend([token.to_owned(), value.to_owned()]);
            } else if selects && matches!(token, "--lib" | "--tests" | "--bins") {
                set.targets.push(token.to_owned());
            } else if selects
                && matches!(token, "--test" | "--bin")
                && let Some(name) = tokens.next()
            {
                set.targets.extend([token.to_owned(), name.to_owned()]);
            } else {
                return Err(ConfigError::NotAFlag {
                    key,
                    token: token.to_owned(),
                });
            }
        }
        Ok(set)
    }

    /// The flags as one line, `default features` for none.
    pub(crate) fn label(&self) -> String {
        if !self.but.is_empty() {
            return format!("all features but {}", self.but.join(", "));
        }
        let words: Vec<&str> = self
            .features
            .iter()
            .chain(&self.targets)
            .map(String::as_str)
            .collect();
        if words.is_empty() {
            "default features".to_owned()
        } else {
            words.join(" ")
        }
    }

    /// The features alone: the doc tests take a test lane's features, not its targets.
    pub(crate) fn features_only(&self) -> Self {
        Self {
            targets: Vec::new(),
            ..self.clone()
        }
    }

    pub(crate) fn args(&self) -> impl Iterator<Item = &str> {
        self.features
            .iter()
            .chain(&self.targets)
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str, targets: Targets) -> FlagSet {
        FlagSet::parse(text, "test", targets).unwrap()
    }

    #[test]
    fn splits_features_from_targets() {
        let set = parse(
            "--lib --test alloc --features golden,gpu  --no-default-features",
            Targets::Allowed,
        );
        assert_eq!(
            set.features,
            ["--features", "golden,gpu", "--no-default-features"]
        );
        assert_eq!(set.targets, ["--lib", "--test", "alloc"]);
        assert_eq!(
            set.label(),
            "--features golden,gpu --no-default-features --lib --test alloc"
        );
        assert_eq!(set.args().count(), 6);

        let empty = parse("", Targets::Refused);
        assert_eq!(empty, FlagSet::default());
        assert_eq!(empty.label(), "default features");
        assert_eq!(
            parse("--features=a", Targets::Refused).features,
            ["--features=a"]
        );
    }

    #[test]
    fn refuses_what_is_not_a_flag() {
        let refused = FlagSet::parse("--lib", "lint", Targets::Refused).unwrap_err();
        assert!(
            matches!(refused, ConfigError::NotAFlag { key: "lint", ref token } if token == "--lib")
        );
        assert!(matches!(
            FlagSet::parse("--features", "test", Targets::Allowed),
            Err(ConfigError::FeaturesNeedsValue { key: "test" })
        ));
        assert!(matches!(
            FlagSet::parse("--test", "test", Targets::Allowed),
            Err(ConfigError::NotAFlag { ref token, .. }) if token == "--test"
        ));
        assert!(matches!(
            FlagSet::parse("--release", "test", Targets::Allowed),
            Err(ConfigError::NotAFlag { .. })
        ));
    }
}
