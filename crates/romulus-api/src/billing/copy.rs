//! The words of the boxes people tick (the Terms, the professional-use
//! attestation, a business's statement, a consumer's requests), per edition
//! of the Terms and per language: one file the web app shows and the API
//! keeps (`web/src/features/billing/checkout-boxes.json`), so a record says
//! which wording was ticked and the confirmation email quotes it. Each
//! edition's `version` is the `copy_version` a request must send and its
//! record keeps; change a word, change the version.

use std::sync::LazyLock;

use serde::Deserialize;

use crate::domain::{Locale, TermsEdition};

/// One language's boxes.
#[derive(Debug, Deserialize)]
pub struct Boxes {
    /// The Terms box; `{version}` is the Terms' version.
    pub terms: String,
    /// The professional edition's attestation at sign-in (Terms 3.5).
    #[serde(default)]
    pub professional_use: Option<String>,
    /// A business's statement at checkout (Terms 3.5).
    pub business: String,
    /// The full edition: a consumer's request to start at once (Article
    /// L221-25)…
    #[serde(default)]
    pub immediate_start: Option<String>,
    /// …and acknowledgement of the withdrawal rule (L221-28, 1°).
    #[serde(default)]
    pub withdrawal: Option<String>,
    /// Under Stripe's "Subscribe" button: it is an order with an
    /// obligation to pay (Terms 3.4 d).
    pub submit: String,
}

#[derive(Debug, Deserialize)]
struct Edition {
    version: String,
    en: Boxes,
    fr: Boxes,
}

#[derive(Debug, Deserialize)]
struct Copy {
    professional: Edition,
    full: Edition,
}

static COPY: LazyLock<Copy> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../../web/src/features/billing/checkout-boxes.json"
    ))
    .expect("checkout-boxes.json is valid (a unit test reads it)")
});

fn edition(edition: TermsEdition) -> &'static Edition {
    match edition {
        TermsEdition::Professional => &COPY.professional,
        TermsEdition::Full => &COPY.full,
    }
}

/// The version of the wording a request must have shown.
pub fn version(of: TermsEdition) -> &'static str {
    &edition(of).version
}

pub fn boxes(of: TermsEdition, locale: Locale) -> &'static Boxes {
    let edition = edition(of);
    match locale {
        Locale::En => &edition.en,
        Locale::Fr => &edition.fr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_boxes_read_in_both_languages_and_editions() {
        assert_ne!(
            version(TermsEdition::Professional),
            version(TermsEdition::Full)
        );
        for locale in [Locale::En, Locale::Fr] {
            for of in [TermsEdition::Professional, TermsEdition::Full] {
                let boxes = boxes(of, locale);
                assert!(boxes.terms.contains("{version}"));
                for text in [&boxes.business, &boxes.submit] {
                    assert!(text.len() > 40 && text.len() <= 1200, "{text}");
                }
            }
            // Professionals attest; consumers ask and acknowledge.
            let pro = boxes(TermsEdition::Professional, locale);
            assert!(pro.professional_use.is_some() && pro.immediate_start.is_none());
            let full = boxes(TermsEdition::Full, locale);
            assert!(full.immediate_start.is_some() && full.withdrawal.is_some());
        }
        assert!(
            boxes(TermsEdition::Professional, Locale::Fr)
                .submit
                .contains("obligation de paiement")
        );
    }
}
