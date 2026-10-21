//! `romulus-api legal-notice`: the email announcing a change to a legal
//! document, at least 30 days before it takes effect (Terms 21.2; Privacy
//! Policy 11; DPA 6.2): the Terms and the Privacy Policy to everyone, the
//! DPA to every organization's admins. Once per person and version
//! (`legal_notices`, queued with the letter: `mail::outbox`), each in their
//! language, with what changes (a summary per language, given by the
//! operator), what they may do (the edition in force's rule: Terms 21.3 for
//! businesses, 21.4 of the full edition for consumers), and a link to the
//! new version, published beforehand: the command refuses a page that does
//! not answer, or the one in force. A new version of the Terms is then
//! accepted again at the next sign-in (`ROMULUS_TERMS_VERSION` moves to it
//! when it takes effect).

use std::time::Duration;

use chrono::{NaiveDate, Utc};
use sqlx::PgPool;
use url::Url;
use uuid::Uuid;

use super::{LegalLinks, words};
use crate::{
    Config,
    domain::{LegalDocument, Locale, TermsEdition},
    mail::{Extras, Letter, Mailer, outbox},
};

/// The notice period the documents promise.
pub const NOTICE_DAYS: i64 = 30;

/// What the operator announces.
#[derive(Debug, Clone)]
pub struct Change {
    pub document: LegalDocument,
    /// The new version, e.g. `1.1`.
    pub version: String,
    /// When it takes effect.
    pub effective: NaiveDate,
    /// What changes, in a few sentences, per language.
    pub summary_en: String,
    pub summary_fr: String,
    /// Where the new version is published (already): in English, and in
    /// French for the Terms and the Privacy Policy (the DPA is English only).
    pub url_en: String,
    pub url_fr: Option<String>,
    /// A change the law requires, which may take effect sooner than 30
    /// days (Terms 21.2).
    pub required_by_law: bool,
    /// Print who would get it, send nothing.
    pub dry_run: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum NoticeError {
    #[error("this server has no Terms (ROMULUS_TERMS_VERSION, ROMULUS_LEGAL_URL)")]
    NoTerms,
    #[error("mail is off: set ROMULUS_SMTP_HOST and ROMULUS_MAIL_FROM")]
    NoMail,
    #[error(
        "the change takes effect in fewer than {NOTICE_DAYS} days: give more notice, or pass --required-by-law for a change the law requires"
    )]
    TooSoon,
    #[error("a summary is needed in both languages (--summary-en, --summary-fr)")]
    NoSummary,
    #[error(
        "the new version's published page is needed: --url-en, and --url-fr for the Terms and the Privacy Policy"
    )]
    NoUrl,
    #[error("{0}: not an http(s) URL")]
    BadUrl(String),
    #[error("{0} is the version in force: link the new version's page")]
    InForce(String),
    #[error("{0} does not answer (status {1}): publish the new version first")]
    NotPublished(String, String),
    #[error("database")]
    Database(#[from] sqlx::Error),
}

/// Who got it (or would have).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Sent {
    pub sent: usize,
    /// Told about this version before.
    pub already: usize,
}

struct Reader {
    id: Uuid,
    email: String,
    locale: Locale,
}

async fn readers(db: &PgPool, document: LegalDocument) -> Result<Vec<Reader>, sqlx::Error> {
    let rows = match document {
        LegalDocument::Terms | LegalDocument::Privacy => sqlx::query!(
            r#"SELECT id, email, locale AS "locale: Locale" FROM users ORDER BY created_at, id"#
        )
        .fetch_all(db)
        .await?
        .into_iter()
        .map(|r| (r.id, r.email, r.locale))
        .collect::<Vec<_>>(),
        LegalDocument::Dpa => sqlx::query!(
            r#"SELECT DISTINCT u.id, u.email, u.locale AS "locale: Locale", u.created_at
               FROM users u JOIN memberships m ON m.user_id = u.id AND m.role = 'admin'
               ORDER BY u.created_at, u.id"#
        )
        .fetch_all(db)
        .await?
        .into_iter()
        .map(|r| (r.id, r.email, r.locale))
        .collect(),
    };
    Ok(rows
        .into_iter()
        .map(|(id, email, locale)| Reader {
            id,
            email,
            locale: locale.unwrap_or_default(),
        })
        .collect())
}

/// Sends the notice of `change` to everyone it concerns who has not had it.
pub async fn send(
    db: &PgPool,
    config: &Config,
    mailer: &Mailer,
    change: &Change,
) -> Result<Sent, NoticeError> {
    let legal = config.legal.as_ref().ok_or(NoticeError::NoTerms)?;
    if !mailer.is_on() && !change.dry_run {
        return Err(NoticeError::NoMail);
    }
    if change.summary_en.trim().is_empty() || change.summary_fr.trim().is_empty() {
        return Err(NoticeError::NoSummary);
    }
    let today = Utc::now().date_naive();
    if (change.effective - today).num_days() < NOTICE_DAYS && !change.required_by_law {
        return Err(NoticeError::TooSoon);
    }
    let links = LegalLinks::of(legal);
    let pages = pages(change, &links)?;
    for page in [&pages.en, &pages.fr] {
        published(page).await?;
    }
    let edition = legal.edition();
    let mut sent = Sent::default();
    for reader in readers(db, change.document).await? {
        if change.dry_run {
            let told = sqlx::query_scalar!(
                r#"SELECT EXISTS (SELECT 1 FROM legal_notices WHERE document = $1 AND version = $2 AND user_id = $3) AS "told!""#,
                change.document as LegalDocument,
                change.version,
                reader.id,
            )
            .fetch_one(db)
            .await?;
            if told {
                sent.already += 1;
            } else {
                sent.sent += 1;
            }
            continue;
        }
        // Recorded and queued together: told once, and the letter never lost.
        let mut tx = db.begin().await?;
        let first = sqlx::query_scalar!(
            "INSERT INTO legal_notices (document, version, user_id) VALUES ($1, $2, $3)
             ON CONFLICT DO NOTHING RETURNING user_id",
            change.document as LegalDocument,
            change.version,
            reader.id,
        )
        .fetch_optional(&mut *tx)
        .await?
        .is_some();
        if !first {
            sent.already += 1;
            continue;
        }
        let email = letter(
            change,
            &links,
            &pages,
            edition,
            &reader.email,
            reader.locale,
        );
        let document = match change.document {
            LegalDocument::Terms => "terms",
            LegalDocument::Privacy => "privacy",
            LegalDocument::Dpa => "dpa",
        };
        let key = format!("legal_notice:{document}:{}:{}", change.version, reader.id);
        outbox::queue(&mut tx, &key, &email).await?;
        tx.commit().await?;
        sent.sent += 1;
    }
    mailer.wake();
    Ok(sent)
}

/// The new version's pages, per language.
struct Pages {
    en: String,
    fr: String,
}

/// Checks the pages given for the new version: http(s), and not the
/// version in force's.
fn pages(change: &Change, links: &LegalLinks) -> Result<Pages, NoticeError> {
    let en = change.url_en.trim();
    let fr = match change.document {
        LegalDocument::Dpa => change.url_fr.as_deref().map_or(en, str::trim),
        LegalDocument::Terms | LegalDocument::Privacy => change
            .url_fr
            .as_deref()
            .map(str::trim)
            .ok_or(NoticeError::NoUrl)?,
    };
    if en.is_empty() || fr.is_empty() {
        return Err(NoticeError::NoUrl);
    }
    let in_force = |docs: &super::DocumentLinks| match change.document {
        LegalDocument::Terms => docs.terms.clone(),
        LegalDocument::Privacy => docs.privacy.clone(),
        LegalDocument::Dpa => docs.dpa.clone(),
    };
    let current = [in_force(&links.en), in_force(&links.fr)];
    for page in [en, fr] {
        let url = Url::parse(page).map_err(|_| NoticeError::BadUrl(page.to_owned()))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(NoticeError::BadUrl(page.to_owned()));
        }
        if current
            .iter()
            .any(|c| c.trim_end_matches('/') == url.as_str().trim_end_matches('/'))
        {
            return Err(NoticeError::InForce(page.to_owned()));
        }
    }
    Ok(Pages {
        en: en.to_owned(),
        fr: fr.to_owned(),
    })
}

/// The page answers (2xx): the new version is published.
async fn published(page: &str) -> Result<(), NoticeError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|err| NoticeError::NotPublished(page.to_owned(), err.to_string()))?;
    match client.get(page).send().await {
        Ok(response) if response.status().is_success() => Ok(()),
        Ok(response) => Err(NoticeError::NotPublished(
            page.to_owned(),
            response.status().as_u16().to_string(),
        )),
        Err(err) => Err(NoticeError::NotPublished(page.to_owned(), err.to_string())),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one letter per document, in both languages"
)]
fn letter(
    change: &Change,
    links: &LegalLinks,
    pages: &Pages,
    edition: TermsEdition,
    to: &str,
    locale: Locale,
) -> crate::mail::Email {
    let url = match locale {
        Locale::En => &pages.en,
        Locale::Fr => &pages.fr,
    };
    let when = words::day(locale, change.effective);
    let contact = &links.contact;
    let v = &change.version;
    let (subject, mut paragraphs, footer) = match (locale, change.document) {
        (Locale::En, LegalDocument::Terms) => (
            format!("romulus's Terms of Service change on {when}"),
            vec![format!(
                "We are changing the Terms of Service of romulus. Version {v} takes effect on {when}."
            )],
            "You get this because you have a romulus account: changes to the Terms are announced at least 30 days ahead (Terms 21).",
        ),
        (Locale::Fr, LegalDocument::Terms) => (
            format!("Les Conditions générales de romulus changent le {when}"),
            vec![format!(
                "Nous modifions les Conditions générales d'utilisation et de vente de romulus. La version {v} s'applique à partir du {when}."
            )],
            "Vous recevez ce message parce que vous avez un compte romulus : toute modification des Conditions est annoncée au moins 30 jours à l'avance (article 21 des Conditions).",
        ),
        (Locale::En, LegalDocument::Privacy) => (
            format!("romulus's Privacy Policy changes on {when}"),
            vec![format!(
                "We are updating the Privacy Policy of romulus. Version {v} applies from {when}."
            )],
            "You get this because you have a romulus account.",
        ),
        (Locale::Fr, LegalDocument::Privacy) => (
            format!("La Politique de confidentialité de romulus change le {when}"),
            vec![format!(
                "Nous mettons à jour la Politique de confidentialité de romulus. La version {v} s'applique à partir du {when}."
            )],
            "Vous recevez ce message parce que vous avez un compte romulus.",
        ),
        (Locale::En, LegalDocument::Dpa) => (
            format!("romulus's Data Processing Agreement changes on {when}"),
            vec![format!(
                "We are changing the Data Processing Agreement of romulus. Version {v} applies from {when}."
            )],
            "You get this because you are an admin of an organization on romulus: changes to the DPA, its subprocessors included, are announced to admins at least 30 days ahead (DPA 6.2).",
        ),
        (Locale::Fr, LegalDocument::Dpa) => (
            format!("L'accord de traitement des données de romulus change le {when}"),
            vec![format!(
                "Nous modifions l'accord de traitement des données (DPA) de romulus. La version {v} s'applique à partir du {when}. Ce document est rédigé en anglais."
            )],
            "Vous recevez ce message parce que vous administrez une organisation sur romulus : toute modification du DPA, sous-traitants compris, est annoncée aux administrateurs au moins 30 jours à l'avance (DPA 6.2).",
        ),
    };
    let summary = match locale {
        Locale::En => &change.summary_en,
        Locale::Fr => &change.summary_fr,
    };
    paragraphs.extend(
        summary
            .split("\n\n")
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_owned),
    );
    paragraphs.push(match change.document {
        // The professional edition (Terms 21.2 and 21.3): the new Terms
        // apply to whoever keeps using the Service; refusing is terminating
        // before they take effect.
        LegalDocument::Terms if edition == TermsEdition::Professional => match locale {
            Locale::En => format!(
                "If you do not accept the change, you can terminate the contract free of charge before {when}: cancel your subscription in your organization's billing settings, or write to {contact}. The prepaid fees for the unused part of the billing period are then refunded. If you keep using romulus after {when}, the new Terms apply to you (Terms 21.3)."
            ),
            Locale::Fr => format!(
                "Si vous n'acceptez pas la modification, vous pouvez résilier le contrat sans frais avant le {when} : résiliez votre abonnement dans les paramètres de facturation de votre organisation, ou écrivez à {contact}. Les sommes payées d'avance pour la partie non écoulée de la période de facturation vous sont alors remboursées. Si vous continuez à utiliser romulus après le {when}, les nouvelles Conditions vous sont applicables (article 21.3 des Conditions)."
            ),
        },
        LegalDocument::Terms => match locale {
            Locale::En => format!(
                "If you use romulus as a consumer, we will ask you to accept the new version when you next sign in; until you do, the current Terms apply until the end of your billing period. If you do not accept the change, you can end your contract at no cost: cancel your subscription in your organization's billing settings, or write to {contact}, and the prepaid fees for the unused part of the period are refunded. If you use romulus for your business, you can terminate before {when}; using romulus after that date means you accept the new Terms."
            ),
            Locale::Fr => format!(
                "Si vous utilisez romulus en tant que consommateur, nous vous demanderons d'accepter la nouvelle version à votre prochaine connexion ; d'ici là, les Conditions actuelles s'appliquent jusqu'à la fin de votre période de facturation. Si vous n'acceptez pas la modification, vous pouvez mettre fin au contrat sans frais : résiliez votre abonnement dans les paramètres de facturation de votre organisation, ou écrivez à {contact}, et la part prépayée de la période non utilisée vous est remboursée. Si vous utilisez romulus pour votre activité professionnelle, vous pouvez résilier avant le {when} ; utiliser romulus après cette date vaut acceptation des nouvelles Conditions."
            ),
        },
        LegalDocument::Privacy => match locale {
            Locale::En => format!(
                "There is nothing to accept: the policy tells you how we process your personal data. Questions or requests about your data: {contact}."
            ),
            Locale::Fr => format!(
                "Il n'y a rien à accepter : la politique vous informe de la façon dont nous traitons vos données personnelles. Questions ou demandes sur vos données : {contact}."
            ),
        },
        LegalDocument::Dpa => match locale {
            Locale::En => format!(
                "You may object to the change on reasonable grounds relating to data protection by writing to {contact} before {when}. If we find no solution together, you can terminate the affected subscription before the change takes effect, and the prepaid fees for the unused part of the period are refunded."
            ),
            Locale::Fr => format!(
                "Vous pouvez vous opposer à la modification pour des motifs raisonnables liés à la protection des données en écrivant à {contact} avant le {when}. Si nous ne trouvons pas de solution ensemble, vous pouvez résilier l'abonnement concerné avant l'entrée en vigueur de la modification, et la part prépayée de la période non utilisée vous est remboursée."
            ),
        },
    });
    let label = match (locale, change.document) {
        (Locale::En, LegalDocument::Terms) => "Read the Terms",
        (Locale::Fr, LegalDocument::Terms) => "Lire les Conditions",
        (Locale::En, LegalDocument::Privacy) => "Read the policy",
        (Locale::Fr, LegalDocument::Privacy) => "Lire la politique",
        (Locale::En, LegalDocument::Dpa) => "Read the DPA",
        (Locale::Fr, LegalDocument::Dpa) => "Lire le DPA",
    };
    Letter {
        kind: "legal_notice",
        to,
        subject,
        paragraphs,
        action: Some((label, url)),
        footer: footer.to_owned(),
    }
    .email_with(Extras {
        lang: locale.tag(),
        ..Extras::default()
    })
}
