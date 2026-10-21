//! The Terms as PDFs, embedded in the binary: the subscription's
//! confirmation attaches the version accepted, in the language it was
//! accepted in (Article L221-13 of the French Consumer Code: a durable
//! medium; a link to a page is not one). Printed from the published pages by
//! `site/tools/legal.mjs` (`just site-legal`), which `just check` verifies
//! were made from the current sources (`site/legal/manifest.json`).
//!
//! A version with no PDF here (a test server's `test`) sends the
//! confirmation without one, linking the Terms instead.

use crate::{
    domain::{Locale, TermsEdition},
    mail::Attachment,
};

type Pdf = (
    &'static str,
    TermsEdition,
    Locale,
    &'static str,
    &'static [u8],
);

/// (version, edition, locale, file name, bytes).
const TERMS: [Pdf; 4] = [
    (
        "1.0",
        TermsEdition::Full,
        Locale::En,
        "romulus-terms-of-service-1.0.pdf",
        include_bytes!("../../legal/terms-1.0-en.pdf"),
    ),
    (
        "1.0",
        TermsEdition::Full,
        Locale::Fr,
        "romulus-conditions-generales-1.0.pdf",
        include_bytes!("../../legal/terms-1.0-fr.pdf"),
    ),
    (
        "1.0",
        TermsEdition::Professional,
        Locale::En,
        "romulus-terms-of-service-professional-1.0.pdf",
        include_bytes!("../../legal/terms-pro-1.0-en.pdf"),
    ),
    (
        "1.0",
        TermsEdition::Professional,
        Locale::Fr,
        "romulus-conditions-generales-professionnels-1.0.pdf",
        include_bytes!("../../legal/terms-pro-1.0-fr.pdf"),
    ),
];

/// The Terms of `version` and `edition` in `locale`, to attach; `None` when
/// this build has no PDF of them.
pub fn terms(version: &str, edition: TermsEdition, locale: Locale) -> Option<Attachment> {
    TERMS
        .iter()
        .find(|(v, e, l, ..)| *v == version && *e == edition && *l == locale)
        .map(|(_, _, _, filename, bytes)| Attachment {
            filename: (*filename).to_owned(),
            content_type: "application/pdf",
            bytes,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_version_is_a_pdf_in_both_languages_and_editions() {
        for (version, edition, ..) in TERMS {
            for locale in [Locale::En, Locale::Fr] {
                let pdf = terms(version, edition, locale).expect("both languages");
                assert!(pdf.bytes.starts_with(b"%PDF-"), "{}", pdf.filename);
                assert!(pdf.bytes.len() > 10_000, "{} looks empty", pdf.filename);
            }
        }
        assert!(terms("test", TermsEdition::Full, Locale::En).is_none());
    }
}
