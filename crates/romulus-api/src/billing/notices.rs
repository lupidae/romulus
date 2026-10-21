//! The emails billing sends, each once (`billing_notices`, keyed by what it
//! is about), each in its reader's language (`users.locale`: English or
//! French; the French version of the Terms prevails for consumers):
//!
//! - **the subscription's confirmation** (Terms 3.4 e; Article L221-13 of
//!   the French Consumer Code), to whoever bought it: the plan, the price,
//!   the period, the renewal, how to cancel, the Terms as a PDF, and for a
//!   consumer the requests ticked and how to withdraw. Once per
//!   subscription;
//! - **a cancellation's confirmation** (Terms 8.1; Article L215-1-1): when
//!   it ends and what that changes. Once per subscription and period;
//! - **a withdrawal's acknowledgement** (Terms 9.4; Article L221-21), to
//!   whoever withdrew, timestamped (`billing::withdrawal`);
//! - **renewal reminders** (Terms 7.5; Article L215-1): [`REMIND_AHEAD`]
//!   before a yearly subscription renews, so one to three months before the
//!   end of the period: the date, the amount, how not to renew, the last
//!   day to cancel in a box, a link to the Cancel button. Once per period;
//! - **a failed payment** (once per invoice), **the grace period ending**
//!   ([`GRACE_WARNING`] before) and **the downgrade** to Free at its end
//!   (audited);
//! - **playground hours**: 80 % and all used, once a month each;
//! - **a price change** ([`price_change`], Terms 7.6), sent by `romulus-api
//!   price-change` to the subscriptions not on the plan table's price.
//!
//! Each is decided once ([`decide`]: its `billing_notices` row) and queued in
//! the same transaction (`mail::outbox`): a letter is never lost to a mail
//! server that was down, and never sent twice. [`run`] looks for what is due
//! every [`NOTICES_EVERY`] (and completes withdrawals Stripe had not
//! finished).

use std::time::Duration;

use chrono::{DateTime, Duration as Span, NaiveDate, Utc};
use sqlx::{PgConnection, PgExecutor, Postgres, Transaction};
use uuid::Uuid;

use super::{
    state::{GRACE_DAYS, Stored},
    stripe::Invoice,
};
use crate::{
    ApiResult, AppState,
    audit::{self, AuditDetails, BillingAudit},
    domain::{AuditAction, Buyer, Interval, Locale, Plan, TermsEdition},
    legal::{LegalLinks, pdf, words},
    mail::{Attachment, Extras, Letter},
};

pub const NOTICES_EVERY: Duration = Duration::from_hours(1);
/// How long before a yearly renewal its reminder goes: more than a month,
/// less than three.
pub const REMIND_AHEAD: i64 = 40;
/// How long before the grace period ends the last warning goes.
pub const GRACE_WARNING: i64 = 3;

/// Whether a yearly renewal at `period_end` is due its reminder at `now`.
pub fn renewal_due(period_end: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now < period_end && now >= period_end - Span::days(REMIND_AHEAD)
}

/// The last day a subscription renewing at `period_end` can be cancelled
/// without renewing: the day before (it renews during the day of
/// `period_end`).
pub fn last_day_to_cancel(period_end: DateTime<Utc>) -> NaiveDate {
    period_end
        .date_naive()
        .pred_opt()
        .unwrap_or(period_end.date_naive())
}

/// Records notice `kind` about `about` for `org`: true the first time.
pub async fn once(
    db: impl PgExecutor<'_>,
    org: Uuid,
    kind: &str,
    about: &str,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query_scalar!(
        "INSERT INTO billing_notices (org_id, kind, about) VALUES ($1, $2, $3)
         ON CONFLICT DO NOTHING RETURNING org_id",
        org,
        kind,
        about,
    )
    .fetch_optional(db)
    .await?
    .is_some())
}

fn date(locale: Locale, at: DateTime<Utc>) -> String {
    words::date(locale, at)
}

fn money(locale: Locale, cents: i64, currency: &str) -> String {
    words::money(locale, cents, currency)
}

/// "month" or "year" in the reader's language, and "monthly"/"yearly".
fn period(locale: Locale, interval: Interval) -> &'static str {
    match (locale, interval) {
        (Locale::En, Interval::Month) => "month",
        (Locale::En, Interval::Year) => "year",
        (Locale::Fr, Interval::Month) => "mois",
        (Locale::Fr, Interval::Year) => "an",
    }
}

fn periodic(locale: Locale, interval: Interval) -> &'static str {
    match (locale, interval) {
        (Locale::En, Interval::Month) => "monthly",
        (Locale::En, Interval::Year) => "yearly",
        (Locale::Fr, Interval::Month) => "mensuelle",
        (Locale::Fr, Interval::Year) => "annuelle",
    }
}

pub(super) struct Org {
    pub(super) name: String,
    pub(super) slug: String,
}

pub(super) async fn org(state: &AppState, id: Uuid) -> Result<Option<Org>, sqlx::Error> {
    org_in(&state.db, id).await
}

async fn org_in(db: impl PgExecutor<'_>, id: Uuid) -> Result<Option<Org>, sqlx::Error> {
    sqlx::query_as!(
        Org,
        "SELECT name, slug FROM organizations WHERE id = $1",
        id
    )
    .fetch_optional(db)
    .await
}

pub(super) fn billing_link(state: &AppState, slug: &str, query: &str) -> String {
    let mut url = state.config.public_url.clone();
    url.set_path(&format!("/app/{slug}/settings/billing"));
    url.set_query((!query.is_empty()).then_some(query));
    url.to_string()
}

/// Someone a letter goes to, in their language.
pub(super) struct Recipient {
    pub(super) email: String,
    pub(super) locale: Locale,
}

/// One reader's letter.
pub(super) struct Draft {
    pub(super) subject: String,
    pub(super) paragraphs: Vec<String>,
    pub(super) action: Option<(&'static str, String)>,
    pub(super) footer: String,
    pub(super) framed: Option<String>,
    pub(super) attachments: Vec<Attachment>,
}

impl Draft {
    fn new(
        subject: String,
        paragraphs: Vec<String>,
        action: Option<(&'static str, String)>,
        footer: String,
    ) -> Self {
        Self {
            subject,
            paragraphs,
            action,
            footer,
            framed: None,
            attachments: Vec::new(),
        }
    }
}

/// Queues one letter in `conn`'s transaction, under `key` and the reader's
/// address (`mail::outbox`; wake the mailer once committed).
pub(super) async fn queue(
    conn: &mut PgConnection,
    key: &str,
    kind: &'static str,
    to: &Recipient,
    draft: Draft,
) -> Result<(), sqlx::Error> {
    let email = Letter {
        kind,
        to: &to.email,
        subject: draft.subject,
        paragraphs: draft.paragraphs,
        action: draft
            .action
            .as_ref()
            .map(|(label, url)| (*label, url.as_str())),
        footer: draft.footer,
    }
    .email_with(Extras {
        lang: to.locale.tag(),
        framed: draft.framed,
        attachments: draft.attachments,
    });
    let key = format!("{key}:{}", crate::email::key(&to.email));
    crate::mail::outbox::queue(conn, &key, &email).await?;
    Ok(())
}

/// A notice decided now, for the first time: its letters are queued in its
/// transaction, and go once it commits ([`Decided::commit`]).
pub struct Decided {
    tx: Transaction<'static, Postgres>,
    key: String,
}

/// Decides notice `kind` about `about` for `org` (`billing_notices`): `None`
/// when it was decided before.
pub async fn decide(
    state: &AppState,
    org: Uuid,
    kind: &str,
    about: &str,
) -> Result<Option<Decided>, sqlx::Error> {
    let mut tx = state.db.begin().await?;
    if !once(&mut *tx, org, kind, about).await? {
        return Ok(None);
    }
    Ok(Some(Decided {
        tx,
        key: format!("billing:{org}:{kind}:{about}"),
    }))
}

impl Decided {
    /// The transaction, for what is recorded with the notice (an audit event).
    pub fn conn(&mut self) -> &mut PgConnection {
        &mut self.tx
    }

    /// Queues one letter to `to`.
    pub(super) async fn send(
        &mut self,
        kind: &'static str,
        to: &Recipient,
        draft: Draft,
    ) -> Result<(), sqlx::Error> {
        queue(&mut self.tx, &self.key, kind, to, draft).await
    }

    /// Queues one letter to every admin of `org_id`, written for each one's
    /// language.
    async fn queue_to_admins(
        &mut self,
        org_id: Uuid,
        kind: &'static str,
        letter: impl Fn(Locale, &Org) -> Draft,
    ) -> Result<(), sqlx::Error> {
        let Some(org) = org_in(&mut *self.tx, org_id).await? else {
            return Ok(());
        };
        for admin in admins(&mut *self.tx, org_id).await? {
            let draft = letter(admin.locale, &org);
            self.send(kind, &admin, draft).await?;
        }
        Ok(())
    }

    /// Records the notice and its letters, and wakes the mailer.
    pub async fn commit(self, state: &AppState) -> Result<(), sqlx::Error> {
        self.tx.commit().await?;
        state.mailer.wake();
        Ok(())
    }
}

/// The admins of `org`, each in their language.
async fn admins(db: impl PgExecutor<'_>, org: Uuid) -> Result<Vec<Recipient>, sqlx::Error> {
    Ok(sqlx::query!(
        r#"SELECT u.email, u.locale AS "locale: Locale"
           FROM memberships m JOIN users u ON u.id = m.user_id
           WHERE m.org_id = $1 AND m.role = 'admin' ORDER BY u.name"#,
        org,
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|r| Recipient {
        email: r.email,
        locale: r.locale.unwrap_or_default(),
    })
    .collect())
}

/// Consumers may buy: the letters cite the consumer law that then applies
/// (the professional edition promises the same, under the Terms).
fn consumer_sales(state: &AppState) -> bool {
    state
        .billing
        .inner()
        .is_some_and(|inner| inner.legal.consumer_sales)
}

fn footer(locale: Locale, org: &Org) -> String {
    match locale {
        Locale::En => format!(
            "You get this because you are an admin of {} on romulus. Billing emails go to every admin of an organization.",
            org.name
        ),
        Locale::Fr => format!(
            "Vous recevez ce message parce que vous administrez {} sur romulus. Les e-mails de facturation sont envoyés à tous les administrateurs d'une organisation.",
            org.name
        ),
    }
}

fn labels(locale: Locale) -> Labels {
    match locale {
        Locale::En => Labels {
            billing: "Billing settings",
            cancel: "Cancel subscription",
            payment: "Update the payment method",
            usage: "See usage",
            plans: "See plans",
        },
        Locale::Fr => Labels {
            billing: "Paramètres de facturation",
            cancel: "Résilier l'abonnement",
            payment: "Mettre à jour le moyen de paiement",
            usage: "Voir la consommation",
            plans: "Voir les offres",
        },
    }
}

struct Labels {
    billing: &'static str,
    cancel: &'static str,
    payment: &'static str,
    usage: &'static str,
    plans: &'static str,
}

/// What renews at the end of a yearly period: the plan as it is, or the
/// change scheduled for then (a downgrade, a switch to monthly) at the plan
/// table's price.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Renewing {
    pub plan: Plan,
    pub interval: Interval,
    /// Cents a period; `None` when unknown.
    pub amount: Option<(i64, String)>,
    /// A change scheduled for the renewal.
    pub changed: bool,
}

impl Renewing {
    /// From the stored subscription: `scheduled` applies when it is due at
    /// the period's end (or before).
    pub fn of(
        plan: Plan,
        amount: Option<(i64, String)>,
        period_end: DateTime<Utc>,
        scheduled: Option<(Plan, Option<Interval>, DateTime<Utc>)>,
    ) -> Self {
        match scheduled {
            Some((next, interval, at))
                if at <= period_end && (next, interval) != (plan, Some(Interval::Year)) =>
            {
                let interval = interval.unwrap_or(Interval::Year);
                Self {
                    plan: next,
                    interval,
                    amount: next
                        .price_cents(interval)
                        .map(|cents| (i64::from(cents), "eur".to_owned())),
                    changed: true,
                }
            }
            _ => Self {
                plan,
                interval: Interval::Year,
                amount,
                changed: false,
            },
        }
    }

    /// The reminder's first sentence.
    pub fn sentence(
        &self,
        locale: Locale,
        current: Plan,
        org: &str,
        period_end: DateTime<Utc>,
    ) -> String {
        let per = period(locale, self.interval);
        let price =
            self.amount
                .as_ref()
                .map_or_else(String::new, |(cents, currency)| match locale {
                    Locale::En => format!(", for {} a {per}", money(locale, *cents, currency)),
                    Locale::Fr => format!(", pour {} par {per}", money(locale, *cents, currency)),
                });
        let end = date(locale, period_end);
        match (locale, self.changed) {
            (Locale::En, true) => format!(
                "The yearly {} plan of {org} ends its period on {end}. As scheduled, it then renews automatically as the {} {} plan{price}.",
                current.name(),
                periodic(locale, self.interval),
                self.plan.name(),
            ),
            (Locale::En, false) => format!(
                "The yearly {} plan of {org} renews automatically on {end}{price}, for another year.",
                self.plan.name(),
            ),
            (Locale::Fr, true) => format!(
                "La période de l'offre {} annuelle de {org} prend fin le {end}. Comme prévu, elle est ensuite reconduite automatiquement en offre {} {}{price}.",
                current.name(),
                self.plan.name(),
                periodic(locale, self.interval),
            ),
            (Locale::Fr, false) => format!(
                "L'offre {} annuelle de {org} est reconduite automatiquement le {end}{price}, pour une nouvelle année.",
                self.plan.name(),
            ),
        }
    }
}

/// The renewal reminder of a yearly subscription (Article L215-1): what
/// renews, as scheduled, for how much, how not to renew, and the last day
/// to cancel in a box.
pub async fn renewal(
    state: &AppState,
    org_id: Uuid,
    current: Plan,
    period_end: DateTime<Utc>,
    renewing: &Renewing,
) -> Result<bool, sqlx::Error> {
    let Some(mut decided) = decide(state, org_id, "renewal", &period_end.to_rfc3339()).await?
    else {
        return Ok(false);
    };
    let last_day = last_day_to_cancel(period_end);
    let consumers = consumer_sales(state);
    decided.queue_to_admins(org_id, "billing_renewal", |locale, org| {
        let cancel = billing_link(state, &org.slug, "action=cancel");
        let end = date(locale, period_end);
        let (subject, paragraphs, framed, why) = match locale {
            Locale::En => (
                format!("{}'s {} plan renews on {end}", org.name, renewing.plan.name()),
                vec![
                    renewing.sentence(locale, current, &org.name, period_end),
                    "If you want it to renew, there is nothing to do.".to_owned(),
                    format!(
                        "If you do not, cancel it by {}: in the organization's billing settings, Cancel subscription. The plan stays until {end}, and {} moves to the Free plan afterwards. Nothing is deleted.",
                        words::day(locale, last_day),
                        org.name,
                    ),
                ],
                format!("Last day to cancel: {}", words::day(locale, last_day)),
                if consumers {
                    "romulus reminds every admin of a yearly renewal more than a month before it (Article L215-1 of the French Consumer Code)."
                } else {
                    "romulus reminds every admin of a yearly renewal more than a month before it (Terms 7.5)."
                },
            ),
            Locale::Fr => (
                format!("L'offre {} de {} est reconduite le {end}", renewing.plan.name(), org.name),
                vec![
                    renewing.sentence(locale, current, &org.name, period_end),
                    "Si vous souhaitez la reconduire, vous n'avez rien à faire.".to_owned(),
                    format!(
                        "Sinon, résiliez-la au plus tard le {} : dans les paramètres de facturation de l'organisation, « Résilier l'abonnement ». L'offre reste active jusqu'au {end}, puis {} passe à l'offre gratuite (Free). Rien n'est supprimé.",
                        words::day(locale, last_day),
                        org.name,
                    ),
                ],
                format!("Dernier jour pour résilier : {}", words::day(locale, last_day)),
                if consumers {
                    "romulus rappelle à chaque administrateur la reconduction d'une offre annuelle plus d'un mois à l'avance (article L215-1 du Code de la consommation)."
                } else {
                    "romulus rappelle à chaque administrateur la reconduction d'une offre annuelle plus d'un mois à l'avance (article 7.5 des Conditions)."
                },
            ),
        };
        Draft {
            framed: Some(framed),
            ..Draft::new(
                subject,
                paragraphs,
                Some((labels(locale).cancel, cancel)),
                format!("{} {why}", footer(locale, org)),
            )
        }
    })
    .await?;
    decided.commit(state).await?;
    Ok(true)
}

/// What a checkout's record says, for the confirmation.
struct Bought {
    email: String,
    locale: Locale,
    accepted_at: DateTime<Utc>,
    terms_version: String,
    edition: TermsEdition,
    buyer: Buyer,
    copy_version: String,
}

async fn bought(state: &AppState, acceptance: Uuid) -> Result<Option<Bought>, sqlx::Error> {
    Ok(sqlx::query!(
        r#"SELECT a.user_email, a.locale AS "locale: Locale", a.accepted_at, a.terms_version,
                  a.edition AS "edition: TermsEdition", a.buyer AS "buyer: Buyer", a.copy_version,
                  u.email AS "current_email?"
           FROM legal_acceptances a LEFT JOIN users u ON u.id = a.user_id
           WHERE a.id = $1 AND a.kind = 'checkout'"#,
        acceptance,
    )
    .fetch_optional(&state.db)
    .await?
    .and_then(|r| {
        Some(Bought {
            // Where the person is now, if they changed address since.
            email: r.current_email.unwrap_or(r.user_email),
            locale: r.locale,
            accepted_at: r.accepted_at,
            terms_version: r.terms_version,
            edition: r.edition,
            buyer: r.buyer?,
            copy_version: r.copy_version.unwrap_or_default(),
        })
    }))
}

/// The subscription's confirmation (Terms 3.4 e; Article L221-13), once per
/// subscription, to whoever bought it (every admin when romulus has no
/// record of the checkout: a subscription made in Stripe's Dashboard).
pub async fn subscription_confirmed(
    state: &AppState,
    org_id: Uuid,
    stored: &Stored,
) -> ApiResult<()> {
    let Some(subscription) = stored.subscription_id.as_deref() else {
        return Ok(());
    };
    let Some(interval) = stored.interval else {
        return Ok(());
    };
    let Some(mut decided) = decide(state, org_id, "subscription_confirmed", subscription).await?
    else {
        return Ok(());
    };
    let Some(org) = org(state, org_id).await? else {
        return Ok(());
    };
    let Some(legal) = state.billing.inner().map(|inner| &inner.legal) else {
        return Ok(());
    };
    let links = LegalLinks::of(legal);
    let bought = match stored.acceptance_id {
        Some(id) => bought(state, id).await?,
        None => None,
    };
    let recipients = match &bought {
        Some(b) => vec![Recipient {
            email: b.email.clone(),
            locale: b.locale,
        }],
        None => admins(&state.db, org_id).await?,
    };
    let started = stored.started_at.unwrap_or_else(Utc::now);
    for to in recipients {
        let locale = to.locale;
        let (version, edition) = bought
            .as_ref()
            .map_or((legal.terms_version.as_str(), legal.edition()), |b| {
                (b.terms_version.as_str(), b.edition)
            });
        let draft = confirmation(
            state,
            &org,
            stored,
            interval,
            started,
            bought.as_ref(),
            (version, edition),
            &links,
            locale,
        );
        decided
            .send("billing_subscription_confirmed", &to, draft)
            .await?;
    }
    decided.commit(state).await?;
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one letter, everything it says, in both languages"
)]
fn confirmation(
    state: &AppState,
    org: &Org,
    stored: &Stored,
    interval: Interval,
    started: DateTime<Utc>,
    bought: Option<&Bought>,
    (version, edition): (&str, TermsEdition),
    links: &LegalLinks,
    locale: Locale,
) -> Draft {
    let plan = stored.plan.name();
    let per = period(locale, interval);
    // « chaque mois », « chaque année » (never « chaque an »).
    let each = match interval {
        Interval::Month => "chaque mois",
        Interval::Year => "chaque année",
    };
    let price = stored
        .amount_cents
        .zip(stored.currency.as_deref())
        .map(|(cents, currency)| money(locale, cents, currency));
    let terms_url = &links.of_locale(locale).terms;
    // Businesses only: prices excluding taxes (Terms 7.3, professional edition).
    let pro = edition == TermsEdition::Professional;
    let attachment = pdf::terms(version, edition, locale);
    let contact = &links.contact;
    let mut paragraphs = Vec::new();
    match locale {
        Locale::En => {
            paragraphs.push(format!(
                "Thank you. This email confirms the subscription of {} to the romulus {plan} plan, which started on {}.",
                org.name,
                words::timestamp(locale, started),
            ));
            paragraphs.push(match &price {
                Some(price) if pro => format!(
                    "Plan: {plan}, {price} a {per}, paid in advance. Prices in euros, excluding taxes; VAT not applicable (art. 293 B du CGI)."
                ),
                Some(price) => format!(
                    "Plan: {plan}, {price} a {per}, paid in advance. Prices are in euros, VAT not applicable (art. 293 B du CGI)."
                ),
                None => format!("Plan: {plan}, billed every {per}, paid in advance."),
            });
            paragraphs.push(format!(
                "Renewal: it renews automatically every {per}{}, at the price in force, until you cancel.{}",
                stored
                    .current_period_end
                    .map_or_else(String::new, |end| format!(", next on {}", date(locale, end))),
                if interval == Interval::Year {
                    " We remind you about 40 days before each yearly renewal."
                } else {
                    ""
                },
            ));
            paragraphs.push(format!(
                "To cancel: at any time, in the organization's billing settings, with Cancel subscription, or by email to {contact}. The plan stays until the end of the period you paid for, then {} moves to the Free plan. Nothing is deleted.",
                org.name
            ));
            paragraphs.push(if attachment.is_some() {
                format!("The Terms of Service you accepted (version {version}) are attached as a PDF; they are also at {terms_url}.")
            } else {
                format!("The Terms of Service you accepted (version {version}) are at {terms_url}.")
            });
        }
        Locale::Fr => {
            paragraphs.push(format!(
                "Merci. Ce message confirme l'abonnement de {} à l'offre {plan} de romulus, qui a commencé le {}.",
                org.name,
                words::timestamp(locale, started),
            ));
            paragraphs.push(match &price {
                Some(price) if pro => format!(
                    "Offre : {plan}, {price} par {per}, payable d'avance. Prix en euros hors taxes ; TVA non applicable (art. 293 B du CGI)."
                ),
                Some(price) => format!(
                    "Offre : {plan}, {price} par {per}, payable d'avance. Prix en euros, TVA non applicable (art. 293 B du CGI)."
                ),
                None => format!("Offre : {plan}, facturée {each}, payable d'avance."),
            });
            paragraphs.push(format!(
                "Reconduction : elle est reconduite automatiquement {each}{}, au prix en vigueur, jusqu'à résiliation.{}",
                stored
                    .current_period_end
                    .map_or_else(String::new, |end| format!(", la prochaine fois le {}", date(locale, end))),
                if interval == Interval::Year {
                    " Nous vous le rappelons environ 40 jours avant chaque reconduction annuelle."
                } else {
                    ""
                },
            ));
            paragraphs.push(format!(
                "Pour résilier : à tout moment, dans les paramètres de facturation de l'organisation, avec « Résilier l'abonnement », ou par e-mail à {contact}. L'offre reste active jusqu'à la fin de la période payée, puis {} passe à l'offre gratuite (Free). Rien n'est supprimé.",
                org.name
            ));
            paragraphs.push(if attachment.is_some() {
                format!("Les Conditions générales que vous avez acceptées (version {version}) sont jointes en PDF ; elles sont aussi disponibles à l'adresse {terms_url}.")
            } else {
                format!("Les Conditions générales que vous avez acceptées (version {version}) sont disponibles à l'adresse {terms_url}.")
            });
        }
    }
    if let Some(b) = bought.filter(|b| b.buyer == Buyer::Consumer) {
        let boxes = super::copy::boxes(b.edition, locale);
        let quote = |text: &Option<String>| text.clone().unwrap_or_default();
        // The last day in Paris (the period runs to its midnight there).
        let until = words::day(locale, super::withdrawal::last_day(started));
        match locale {
            Locale::En => {
                paragraphs.push(format!(
                    "As a consumer, you made these two requests at checkout, on {} (wording {}):",
                    words::timestamp(locale, b.accepted_at),
                    b.copy_version,
                ));
                paragraphs.push(format!("“{}”", quote(&boxes.immediate_start)));
                paragraphs.push(format!("“{}”", quote(&boxes.withdrawal)));
                paragraphs.push(format!(
                    "You can withdraw until the end of {until} (14 days): in the billing settings, Withdraw from contract here, then Confirm withdrawal; or by email to {contact}; or with the model form in section 9.4 of the Terms. You then pay only for the days of service until you withdraw, and we refund the rest within 14 days, with the payment method you used. {} then moves to the Free plan; nothing is deleted.",
                    org.name
                ));
            }
            Locale::Fr => {
                paragraphs.push(format!(
                    "En tant que consommateur, vous avez formulé ces deux demandes lors du paiement, le {} (formulation {}) :",
                    words::timestamp(locale, b.accepted_at),
                    b.copy_version,
                ));
                paragraphs.push(format!("« {} »", quote(&boxes.immediate_start)));
                paragraphs.push(format!("« {} »", quote(&boxes.withdrawal)));
                paragraphs.push(format!(
                    "Vous pouvez vous rétracter jusqu'au {until} inclus (14 jours) : dans les paramètres de facturation, « Renoncer au contrat ici », puis « Confirmer la rétractation » ; ou par e-mail à {contact} ; ou avec le formulaire type de l'article 9.4 des Conditions. Vous ne payez alors que les jours de service jusqu'à votre rétractation, et nous vous remboursons le reste sous 14 jours, avec le moyen de paiement utilisé. {} passe ensuite à l'offre gratuite (Free) ; rien n'est supprimé.",
                    org.name
                ));
            }
        }
    }
    let (subject, foot) = match locale {
        Locale::En => (
            format!("Your romulus {plan} plan for {} has started", org.name),
            format!(
                "You get this because you subscribed {} to a romulus plan. Stripe sends the receipt and the invoice separately. Questions: {contact}.",
                org.name
            ),
        ),
        Locale::Fr => (
            format!("Votre offre romulus {plan} pour {} a commencé", org.name),
            format!(
                "Vous recevez ce message parce que vous avez souscrit une offre romulus pour {}. Stripe envoie le reçu et la facture séparément. Questions : {contact}.",
                org.name
            ),
        ),
    };
    Draft {
        attachments: attachment.into_iter().collect(),
        ..Draft::new(
            subject,
            paragraphs,
            Some((labels(locale).billing, billing_link(state, &org.slug, ""))),
            foot,
        )
    }
}

/// A cancellation confirmed (Terms 8.1; Article L215-1-1): when the plan
/// ends and what that changes. Once per subscription and period.
pub async fn cancellation_confirmed(
    state: &AppState,
    org_id: Uuid,
    stored: &Stored,
) -> ApiResult<()> {
    let (Some(subscription), Some(ends)) =
        (stored.subscription_id.as_deref(), stored.current_period_end)
    else {
        return Ok(());
    };
    let Some(mut decided) = decide(
        state,
        org_id,
        "cancellation_confirmed",
        &format!("{subscription}:{}", ends.timestamp()),
    )
    .await?
    else {
        return Ok(());
    };
    let plan = stored.plan;
    let consumers = consumer_sales(state);
    decided.queue_to_admins(org_id, "billing_cancellation_confirmed", |locale, org| {
        let end = date(locale, ends);
        let (subject, paragraphs, why) = match locale {
            Locale::En => (
                format!("{}'s {} plan is cancelled: it ends on {end}", org.name, plan.name()),
                vec![
                    format!("This confirms that the {} subscription of {} is cancelled.", plan.name(), org.name),
                    format!("It ends on {end}. Until then you keep the paid features, and nothing more is charged."),
                    format!(
                        "Then {} moves to the Free plan. Nothing is deleted: what is over the Free plan's caps stays readable, and creating more waits until a paid plan is back. Versions beyond the Free plan's history are hidden, not deleted.",
                        org.name
                    ),
                    format!("Changed your mind? Renew it from Manage billing in the billing settings before {end}."),
                ],
                if consumers {
                    "romulus confirms every cancellation by email (Article L215-1-1 of the French Consumer Code)."
                } else {
                    "romulus confirms every cancellation by email (Terms 8.1)."
                },
            ),
            Locale::Fr => (
                format!("L'offre {} de {} est résiliée : elle prend fin le {end}", plan.name(), org.name),
                vec![
                    format!("Ce message confirme la résiliation de l'abonnement {} de {}.", plan.name(), org.name),
                    format!("Il prend fin le {end}. D'ici là, vous gardez les fonctionnalités payantes, et rien de plus n'est prélevé."),
                    format!(
                        "{} passe ensuite à l'offre gratuite (Free). Rien n'est supprimé : ce qui dépasse les limites de l'offre gratuite reste consultable, et créer davantage attend le retour d'une offre payante. Les versions au-delà de l'historique de l'offre gratuite sont masquées, pas supprimées.",
                        org.name
                    ),
                    format!("Vous changez d'avis ? Reprenez l'abonnement depuis « Gérer la facturation » dans les paramètres de facturation avant le {end}."),
                ],
                if consumers {
                    "romulus confirme chaque résiliation par e-mail (article L215-1-1 du Code de la consommation)."
                } else {
                    "romulus confirme chaque résiliation par e-mail (article 8.1 des Conditions)."
                },
            ),
        };
        Draft::new(
            subject,
            paragraphs,
            Some((labels(locale).billing, billing_link(state, &org.slug, ""))),
            format!("{} {why}", footer(locale, org)),
        )
    })
    .await?;
    decided.commit(state).await?;
    Ok(())
}

/// A second subscription was cancelled and refunded (two checkouts paid at
/// once): the admins are told what happened to their money.
pub async fn duplicate_cancelled(
    state: &AppState,
    org_id: Uuid,
    subscription: &str,
    refund: Option<&super::stripe::Refund>,
) -> ApiResult<()> {
    let Some(mut decided) = decide(state, org_id, "duplicate_cancelled", subscription).await?
    else {
        return Ok(());
    };
    decided.queue_to_admins(org_id, "billing_duplicate_cancelled", |locale, org| {
        let refunded = match (locale, refund) {
            (Locale::En, None) => "Nothing had been charged for it.".to_owned(),
            (Locale::En, Some(r)) => format!(
                "Its payment of {} is refunded in full: it shows on your statement within days.",
                money(locale, r.amount, &r.currency)
            ),
            (Locale::Fr, None) => "Rien n'avait été prélevé pour lui.".to_owned(),
            (Locale::Fr, Some(r)) => format!(
                "Son paiement de {} est intégralement remboursé : il apparaîtra sur votre relevé sous quelques jours.",
                money(locale, r.amount, &r.currency)
            ),
        };
        let (subject, first) = match locale {
            Locale::En => (
                format!("A second subscription for {} was cancelled", org.name),
                format!(
                    "Two checkouts for {} were paid at nearly the same moment, which started a second subscription. The first one stays; romulus cancelled the second at once.",
                    org.name
                ),
            ),
            Locale::Fr => (
                format!("Un second abonnement pour {} a été annulé", org.name),
                format!(
                    "Deux paiements pour {} ont été validés presque au même moment, ce qui a ouvert un second abonnement. Le premier est conservé ; romulus a annulé le second aussitôt.",
                    org.name
                ),
            ),
        };
        Draft::new(
            subject,
            vec![first, refunded],
            Some((labels(locale).billing, billing_link(state, &org.slug, ""))),
            footer(locale, org),
        )
    })
    .await?;
    decided.commit(state).await?;
    Ok(())
}

/// A payment failed: once per invoice.
pub async fn payment_failed(
    state: &AppState,
    org_id: Uuid,
    invoice: &Invoice,
    grace_until: Option<DateTime<Utc>>,
) -> ApiResult<()> {
    let Some(mut decided) = decide(state, org_id, "payment_failed", &invoice.id).await? else {
        return Ok(());
    };
    decided.queue_to_admins(org_id, "billing_payment_failed", |locale, org| {
        let amount = money(locale, invoice.total, &invoice.currency);
        let (subject, paragraphs) = match locale {
            Locale::En => {
                let until = grace_until.map_or_else(
                    || "for now".to_owned(),
                    |until| format!("until {}", date(locale, until)),
                );
                (
                    format!("A payment for {} failed", org.name),
                    vec![
                        format!("The payment of {amount} for {} did not go through. Stripe will try again.", org.name),
                        format!(
                            "The plan stays {until}. To keep it after that, update the payment method; otherwise {} moves to the Free plan. Nothing is deleted.",
                            org.name
                        ),
                    ],
                )
            }
            Locale::Fr => {
                let until = grace_until.map_or_else(
                    || "pour l'instant".to_owned(),
                    |until| format!("jusqu'au {}", date(locale, until)),
                );
                (
                    format!("Un paiement pour {} a échoué", org.name),
                    vec![
                        format!("Le paiement de {amount} pour {} n'a pas abouti. Stripe va réessayer.", org.name),
                        format!(
                            "L'offre reste active {until}. Pour la garder au-delà, mettez à jour le moyen de paiement ; sinon {} passe à l'offre gratuite (Free). Rien n'est supprimé.",
                            org.name
                        ),
                    ],
                )
            }
        };
        Draft::new(
            subject,
            paragraphs,
            Some((labels(locale).payment, billing_link(state, &org.slug, ""))),
            footer(locale, org),
        )
    })
    .await?;
    decided.commit(state).await?;
    Ok(())
}

/// Playground hours at 80 % (once a month).
pub async fn hours_warning(
    state: &AppState,
    org_id: Uuid,
    plan: Plan,
    used_seconds: i64,
    month: NaiveDate,
    next: NaiveDate,
) -> ApiResult<()> {
    let key = month.format("%Y-%m").to_string();
    let Some(mut decided) = decide(state, org_id, "hours_80", &key).await? else {
        return Ok(());
    };
    let hours = plan.limits().playground_hours;
    let used = used_seconds / 3600;
    decided.queue_to_admins(org_id, "billing_hours_warning", |locale, org| {
        let back = words::day_of_year(locale, next);
        let (subject, paragraphs) = match locale {
            Locale::En => (
                format!("{} used 80 % of its playground hours", org.name),
                vec![
                    format!(
                        "{} ran playgrounds for about {used} of its {hours} hours this month ({} plan).",
                        org.name,
                        plan.name()
                    ),
                    format!("Once all are used, running playgrounds stop and new ones wait until {back}. Their data stays."),
                ],
            ),
            Locale::Fr => (
                format!("{} a utilisé 80 % de ses heures de playground", org.name),
                vec![
                    format!(
                        "{} a fait tourner des playgrounds environ {used} de ses {hours} heures ce mois-ci (offre {}).",
                        org.name,
                        plan.name()
                    ),
                    format!("Une fois toutes utilisées, les playgrounds en cours s'arrêtent et les nouveaux attendent le {back}. Leurs données restent."),
                ],
            ),
        };
        Draft::new(
            subject,
            paragraphs,
            Some((labels(locale).usage, billing_link(state, &org.slug, ""))),
            footer(locale, org),
        )
    })
    .await?;
    decided.commit(state).await?;
    Ok(())
}

/// Playground hours all used: queued with the caller's decision (once a
/// month).
pub async fn hours_used_up(
    state: &AppState,
    decided: &mut Decided,
    org_id: Uuid,
    plan: Plan,
    month: NaiveDate,
    next: NaiveDate,
) -> Result<(), sqlx::Error> {
    decided.queue_to_admins(org_id, "billing_hours_used_up", |locale, org| {
        let back = words::day_of_year(locale, next);
        let name = words::month_name(locale, month);
        let hours = plan.limits().playground_hours;
        let (subject, paragraphs) = match locale {
            Locale::En => (
                format!("{} used its playground hours for {name}", org.name),
                vec![
                    format!(
                        "{} used the {hours} playground hours of its {} plan this month. Its running playgrounds were stopped; their data stays until they expire.",
                        org.name,
                        plan.name()
                    ),
                    format!("Playgrounds and migration tests start again on {back}, or now with a larger plan."),
                ],
            ),
            Locale::Fr => (
                format!("{} a utilisé ses heures de playground de {name}", org.name),
                vec![
                    format!(
                        "{} a utilisé les {hours} heures de playground de son offre {} ce mois-ci. Ses playgrounds en cours ont été arrêtés ; leurs données restent jusqu'à leur expiration.",
                        org.name,
                        plan.name()
                    ),
                    format!("Les playgrounds et les tests de migration reprennent le {back}, ou dès maintenant avec une offre supérieure."),
                ],
            ),
        };
        Draft::new(
            subject,
            paragraphs,
            Some((labels(locale).plans, billing_link(state, &org.slug, ""))),
            footer(locale, org),
        )
    })
    .await
}

/// A plan's price changes at a renewal (Terms 7.6: 30 days ahead at least).
/// `romulus-api price-change` sends it, once per organization and change
/// (`change`: e.g. `team_yearly:59000`).
pub async fn price_change(
    state: &AppState,
    org_id: Uuid,
    change: &str,
    plan: Plan,
    interval: Interval,
    new_cents: i64,
    from: DateTime<Utc>,
) -> ApiResult<bool> {
    let Some(mut decided) = decide(state, org_id, "price_change", change).await? else {
        return Ok(false);
    };
    let last_day = last_day_to_cancel(from);
    decided.queue_to_admins(org_id, "billing_price_change", |locale, org| {
        let per = period(locale, interval);
        let price = money(locale, new_cents, "eur");
        let when = date(locale, from);
        let (subject, paragraphs, framed) = match locale {
            Locale::En => (
                format!("The price of {}'s {} plan changes on {when}", org.name, plan.name()),
                vec![
                    format!("From your renewal on {when}, the {} plan costs {price} a {per}.", plan.name()),
                    "If you do not want to continue at this price, cancel before then, at no cost: the plan stays at its current price until the end of the period you paid for.".to_owned(),
                ],
                format!("Last day to cancel: {}", words::day(locale, last_day)),
            ),
            Locale::Fr => (
                format!("Le prix de l'offre {} de {} change le {when}", plan.name(), org.name),
                vec![
                    format!("À partir de votre reconduction du {when}, l'offre {} coûte {price} par {per}.", plan.name()),
                    "Si vous ne souhaitez pas continuer à ce prix, résiliez avant cette date, sans frais : l'offre reste à son prix actuel jusqu'à la fin de la période payée.".to_owned(),
                ],
                format!("Dernier jour pour résilier : {}", words::day(locale, last_day)),
            ),
        };
        Draft {
            framed: Some(framed),
            ..Draft::new(
                subject,
                paragraphs,
                Some((labels(locale).cancel, billing_link(state, &org.slug, "action=cancel"))),
                footer(locale, org),
            )
        }
    })
    .await?;
    decided.commit(state).await?;
    Ok(true)
}

/// A subscription told of its new price (`romulus-api price-change`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceNotice {
    pub org: String,
    pub plan: Plan,
    pub interval: Interval,
    pub cents: i64,
    /// The renewal from which the new price applies.
    pub from: DateTime<Utc>,
    /// Sent now (false: told before, or a dry run).
    pub sent: bool,
}

/// The first renewal of a period ending at `period_end` that is at least
/// `threshold`: the period's end, or a whole number of periods after it,
/// each counted from `period_end` itself (a period ending on the 31st
/// renews on the last day of February, then on 31 March again, as Stripe
/// does: never drifting to the 28th).
pub fn first_renewal_after(
    period_end: DateTime<Utc>,
    interval: Interval,
    threshold: DateTime<Utc>,
) -> DateTime<Utc> {
    let months = match interval {
        Interval::Month => 1,
        Interval::Year => 12,
    };
    let mut renewal = period_end;
    let mut periods = 0;
    while renewal < threshold {
        periods += months;
        match period_end.checked_add_months(chrono::Months::new(periods)) {
            Some(next) => renewal = next,
            None => break,
        }
    }
    renewal
}

/// Tells every running subscription whose price is not the plan table's of
/// its new price (Terms 7.6), from its first renewal at least 30 days away
/// (and not before `not_before`): after the plan table and `stripe-setup`
/// changed it, and before the subscriptions are moved to the new price in
/// Stripe on those dates. `plan` narrows it to one plan.
pub async fn price_changes(
    state: &AppState,
    plan: Option<Plan>,
    not_before: Option<DateTime<Utc>>,
    dry_run: bool,
) -> ApiResult<Vec<PriceNotice>> {
    let now = Utc::now();
    let threshold = (now + Span::days(30)).max(not_before.unwrap_or(now));
    let rows = sqlx::query!(
        r#"SELECT b.org_id, o.slug, b.plan AS "plan: Plan", b.interval AS "interval!: Interval",
                  b.amount_cents AS "amount!", b.current_period_end AS "period_end!"
           FROM org_billing b JOIN organizations o ON o.id = b.org_id
           WHERE b.status IN ('active', 'trialing', 'past_due') AND b.withdrawn_at IS NULL
             AND NOT b.cancel_at_period_end AND b.subscription_id IS NOT NULL
             AND b.interval IS NOT NULL AND b.amount_cents IS NOT NULL
             AND b.current_period_end IS NOT NULL AND b.plan <> 'free'
           ORDER BY o.slug"#
    )
    .fetch_all(&state.db)
    .await?;
    let mut notices = Vec::new();
    for r in rows {
        if plan.is_some_and(|p| p != r.plan) {
            continue;
        }
        let Some(cents) = r.plan.price_cents(r.interval).map(i64::from) else {
            continue;
        };
        if cents == r.amount {
            continue;
        }
        let from = first_renewal_after(r.period_end, r.interval, threshold);
        let key = format!(
            "{}:{cents}",
            r.plan.lookup_key(r.interval).unwrap_or_default()
        );
        let sent = !dry_run
            && price_change(state, r.org_id, &key, r.plan, r.interval, cents, from).await?;
        notices.push(PriceNotice {
            org: r.slug,
            plan: r.plan,
            interval: r.interval,
            cents,
            from,
            sent,
        });
    }
    Ok(notices)
}

/// Looks for due notices every [`NOTICES_EVERY`] until aborted.
pub fn run(state: AppState, every: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        loop {
            tick.tick().await;
            if let Err(err) = due(&state, Utc::now()).await {
                tracing::warn!(error = %err, "billing: notices failed");
            }
        }
    })
}

/// What is due at `now`: renewal reminders, grace warnings, downgrades, and
/// withdrawals Stripe had not finished.
pub async fn due(state: &AppState, now: DateTime<Utc>) -> ApiResult<()> {
    if !state.billing.is_on() {
        return Ok(());
    }
    // Stripe retries a delivery for three days: event ids older than 90 are
    // of no use.
    sqlx::query!("DELETE FROM billing_events WHERE received_at < now() - interval '90 days'")
        .execute(&state.db)
        .await?;
    super::withdrawal::complete_pending(state).await?;
    let renewals = sqlx::query!(
        r#"SELECT org_id, plan AS "plan: Plan", current_period_end AS "period_end!", amount_cents, currency,
                  scheduled_plan AS "scheduled_plan: Plan",
                  scheduled_interval AS "scheduled_interval: Interval", scheduled_at
           FROM org_billing
           WHERE interval = 'year' AND status IN ('active', 'trialing') AND NOT cancel_at_period_end
             AND withdrawn_at IS NULL
             AND current_period_end > $1 AND current_period_end <= $1 + make_interval(days => $2)"#,
        now,
        i32::try_from(REMIND_AHEAD).unwrap_or(40),
    )
    .fetch_all(&state.db)
    .await?;
    for r in renewals {
        if !renewal_due(r.period_end, now) {
            continue;
        }
        let renewing = Renewing::of(
            r.plan,
            r.amount_cents.zip(r.currency),
            r.period_end,
            r.scheduled_plan
                .zip(r.scheduled_at)
                .map(|(plan, at)| (plan, r.scheduled_interval, at)),
        );
        if renewal(state, r.org_id, r.plan, r.period_end, &renewing).await?
            && r.period_end - now < Span::days(30)
        {
            tracing::warn!(org = %r.org_id, "billing: a renewal reminder sent less than 30 days ahead");
        }
    }

    let overdue = sqlx::query!(
        r#"SELECT org_id, plan AS "plan: Plan", past_due_since AS "since!" FROM org_billing
           WHERE status IN ('past_due', 'unpaid') AND past_due_since IS NOT NULL AND withdrawn_at IS NULL"#
    )
    .fetch_all(&state.db)
    .await?;
    for r in overdue {
        let until = r.since + Span::days(GRACE_DAYS);
        let key = r.since.to_rfc3339();
        if now >= until {
            if let Some(decided) = decide(state, r.org_id, "downgraded", &key).await? {
                downgraded(state, decided, r.org_id, r.plan, r.since).await?;
            }
        } else if now >= until - Span::days(GRACE_WARNING)
            && let Some(mut decided) = decide(state, r.org_id, "grace_ending", &key).await?
        {
            decided.queue_to_admins(r.org_id, "billing_grace_ending", |locale, org| {
                let when = date(locale, until);
                let (subject, paragraphs) = match locale {
                    Locale::En => (
                        format!("{} moves to the Free plan on {when}", org.name),
                        vec![
                            format!(
                                "A payment for {}'s {} plan is still outstanding. Unless it is paid by {when}, the organization moves to the Free plan.",
                                org.name,
                                r.plan.name(),
                            ),
                            "Nothing is deleted: what is over the Free plan's caps stays readable, and creating more waits until the plan is back.".to_owned(),
                        ],
                    ),
                    Locale::Fr => (
                        format!("{} passe à l'offre gratuite (Free) le {when}", org.name),
                        vec![
                            format!(
                                "Un paiement de l'offre {} de {} reste dû. S'il n'est pas réglé d'ici le {when}, l'organisation passe à l'offre gratuite (Free).",
                                r.plan.name(),
                                org.name,
                            ),
                            "Rien n'est supprimé : ce qui dépasse les limites de l'offre gratuite reste consultable, et créer davantage attend le retour de l'offre.".to_owned(),
                        ],
                    ),
                };
                Draft::new(
                    subject,
                    paragraphs,
                    Some((labels(locale).payment, billing_link(state, &org.slug, ""))),
                    footer(locale, org),
                )
            })
            .await?;
            decided.commit(state).await?;
        }
    }
    Ok(())
}

/// The grace period ended: audited, and the admins and the operators told.
async fn downgraded(
    state: &AppState,
    mut decided: Decided,
    org_id: Uuid,
    plan: Plan,
    since: DateTime<Utc>,
) -> ApiResult<()> {
    audit::record(
        decided.conn(),
        &audit::Entry {
            org_id,
            database_id: None,
            action: AuditAction::PlanDowngraded,
            actor_id: None,
            actor_name: None,
            version_number: None,
            reason: None,
            details: AuditDetails {
                billing: Some(BillingAudit {
                    from_plan: Some(plan),
                    plan: Some(Plan::Free),
                    ..BillingAudit::default()
                }),
                ..AuditDetails::default()
            },
        },
    )
    .await?;
    crate::operator::billing::grace_ended(decided.conn(), state, org_id, since).await?;
    decided.queue_to_admins(org_id, "billing_downgraded", |locale, org| {
        let (subject, paragraphs) = match locale {
            Locale::En => (
                format!("{} is on the Free plan", org.name),
                vec![
                    format!(
                        "The payment for {}'s {} plan was not made within {GRACE_DAYS} days, so the organization is on the Free plan now.",
                        org.name,
                        plan.name()
                    ),
                    "Nothing was deleted. What is over the Free plan's caps stays readable; creating more waits until the plan is back. Paying the outstanding invoice brings the plan back at once.".to_owned(),
                ],
            ),
            Locale::Fr => (
                format!("{} est passée à l'offre gratuite (Free)", org.name),
                vec![
                    format!(
                        "Le paiement de l'offre {} de {} n'a pas été effectué dans les {GRACE_DAYS} jours : l'organisation est désormais sur l'offre gratuite (Free).",
                        plan.name(),
                        org.name,
                    ),
                    "Rien n'a été supprimé. Ce qui dépasse les limites de l'offre gratuite reste consultable ; créer davantage attend le retour de l'offre. Régler la facture en attente rétablit l'offre aussitôt.".to_owned(),
                ],
            ),
        };
        Draft::new(
            subject,
            paragraphs,
            Some((labels(locale).billing, billing_link(state, &org.slug, ""))),
            footer(locale, org),
        )
    })
    .await?;
    decided.commit(state).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn the_reminder_says_what_renews_a_scheduled_change_included() {
        let end = Utc.with_ymd_and_hms(2027, 3, 1, 0, 0, 0).unwrap();
        let same = Renewing::of(Plan::Team, Some((49_000, "eur".into())), end, None);
        assert_eq!(
            same.sentence(Locale::En, Plan::Team, "Acme", end),
            "The yearly Team plan of Acme renews automatically on March 1, 2027, for €490.00 a year, for another year."
        );
        assert_eq!(
            same.sentence(Locale::Fr, Plan::Team, "Acme", end),
            "L'offre Team annuelle de Acme est reconduite automatiquement le 1er mars 2027, pour 490,00\u{a0}€ par an, pour une nouvelle année."
        );
        // A downgrade to monthly Starter at the period's end.
        let down = Renewing::of(
            Plan::Team,
            Some((49_000, "eur".into())),
            end,
            Some((Plan::Starter, Some(Interval::Month), end)),
        );
        assert_eq!(
            (down.plan, down.interval, down.changed),
            (Plan::Starter, Interval::Month, true)
        );
        assert_eq!(
            down.sentence(Locale::En, Plan::Team, "Acme", end),
            "The yearly Team plan of Acme ends its period on March 1, 2027. As scheduled, it then renews automatically as the monthly Starter plan, for €19.00 a month."
        );
        // A yearly downgrade keeps "a year".
        let yearly = Renewing::of(
            Plan::Business,
            None,
            end,
            Some((Plan::Team, Some(Interval::Year), end)),
        );
        assert_eq!(yearly.amount, Some((49_000, "eur".into())));
        assert!(
            yearly
                .sentence(Locale::En, Plan::Business, "Acme", end)
                .contains("the yearly Team plan, for €490.00 a year")
        );
        // A change scheduled after this renewal is not this renewal's.
        let later = Renewing::of(
            Plan::Team,
            Some((49_000, "eur".into())),
            end,
            Some((Plan::Starter, Some(Interval::Month), end + Span::days(365))),
        );
        assert!(!later.changed);
    }

    #[test]
    fn a_yearly_renewal_is_reminded_between_forty_days_and_its_date() {
        let end = Utc.with_ymd_and_hms(2027, 3, 1, 0, 0, 0).unwrap();
        assert!(!renewal_due(end, end - Span::days(41)));
        assert!(renewal_due(end, end - Span::days(40)));
        assert!(renewal_due(end, end - Span::days(31)));
        // Late (the job was down): still before the renewal, still sent.
        assert!(renewal_due(end, end - Span::days(2)));
        assert!(!renewal_due(end, end));
        // Within the window the law sets: no earlier than three months, no
        // later than one month ahead.
        const { assert!(REMIND_AHEAD > 30 && REMIND_AHEAD < 90) };
    }

    #[test]
    fn a_new_price_applies_from_the_first_renewal_thirty_days_away() {
        let end = Utc.with_ymd_and_hms(2026, 11, 5, 8, 0, 0).unwrap();
        let soon = Utc.with_ymd_and_hms(2026, 11, 20, 0, 0, 0).unwrap();
        assert_eq!(
            first_renewal_after(end, Interval::Month, soon),
            Utc.with_ymd_and_hms(2026, 12, 5, 8, 0, 0).unwrap()
        );
        assert_eq!(
            first_renewal_after(end, Interval::Year, soon),
            Utc.with_ymd_and_hms(2027, 11, 5, 8, 0, 0).unwrap()
        );
        assert_eq!(first_renewal_after(end, Interval::Month, end), end);
        // Anchored on the 31st: February's last day, then the 31st again.
        let jan = Utc.with_ymd_and_hms(2027, 1, 31, 8, 0, 0).unwrap();
        assert_eq!(
            first_renewal_after(jan, Interval::Month, jan + Span::days(1)),
            Utc.with_ymd_and_hms(2027, 2, 28, 8, 0, 0).unwrap()
        );
        assert_eq!(
            first_renewal_after(jan, Interval::Month, jan + Span::days(40)),
            Utc.with_ymd_and_hms(2027, 3, 31, 8, 0, 0).unwrap()
        );
        assert_eq!(
            first_renewal_after(jan, Interval::Month, jan + Span::days(100)),
            Utc.with_ymd_and_hms(2027, 5, 31, 8, 0, 0).unwrap()
        );
    }

    #[test]
    fn the_last_day_to_cancel_is_the_day_before_the_renewal() {
        let end = Utc.with_ymd_and_hms(2027, 3, 1, 14, 23, 0).unwrap();
        assert_eq!(
            last_day_to_cancel(end),
            NaiveDate::from_ymd_opt(2027, 2, 28).unwrap()
        );
    }
}
