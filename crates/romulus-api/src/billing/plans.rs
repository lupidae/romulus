//! The plan table: what each hosted plan costs and allows. The one place
//! the numbers live; enforcement ([`super::caps`]), the billing page and the
//! Stripe setup (`romulus-api stripe-setup`, its prices and lookup keys)
//! all read it, so changing a cap or a price is an edit here.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::domain::{Interval, Plan, PlaygroundSize};

/// What a plan costs and allows. `None` is unlimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Limits {
    /// Euro cents a month; a year costs [`YEARLY_MONTHS`] of them.
    pub monthly_cents: u32,
    pub members: Option<u32>,
    pub databases: u32,
    pub versions_per_database: Option<u32>,
    /// Tables and views (materialized and foreign ones too) in one version.
    pub schema_objects: u32,
    /// Versions older than this are hidden (never deleted).
    pub history_days: Option<u32>,
    /// The largest playground (and migration test) it makes.
    pub playground_size: PlaygroundSize,
    /// Running playground time a calendar month (UTC), tests included.
    pub playground_hours: u32,
    /// The most approvals publishing a draft can be made to need (0: the
    /// setting is off).
    pub approvals: u8,
    /// Admins may publish without the approvals, saying why.
    pub approvals_override: bool,
}

/// A year is paid as ten months: two free.
pub const YEARLY_MONTHS: u32 = 10;

const FREE: Limits = Limits {
    monthly_cents: 0,
    members: Some(1),
    databases: 1,
    versions_per_database: Some(50),
    schema_objects: 50,
    history_days: Some(90),
    playground_size: PlaygroundSize::Small,
    playground_hours: 2,
    approvals: 0,
    approvals_override: false,
};

const STARTER: Limits = Limits {
    monthly_cents: 1_900,
    members: None,
    databases: 3,
    versions_per_database: Some(500),
    schema_objects: 200,
    history_days: Some(365),
    playground_size: PlaygroundSize::Small,
    playground_hours: 20,
    approvals: 1,
    approvals_override: false,
};

const TEAM: Limits = Limits {
    monthly_cents: 4_900,
    members: None,
    databases: 10,
    versions_per_database: None,
    schema_objects: 1_000,
    history_days: None,
    playground_size: PlaygroundSize::Medium,
    playground_hours: 80,
    approvals: 10,
    approvals_override: false,
};

const BUSINESS: Limits = Limits {
    monthly_cents: 14_900,
    members: None,
    databases: 50,
    versions_per_database: None,
    schema_objects: 5_000,
    history_days: None,
    playground_size: PlaygroundSize::Medium,
    playground_hours: 300,
    approvals: 10,
    approvals_override: true,
};

/// Every plan, cheapest first.
pub const ALL: [Plan; 4] = [Plan::Free, Plan::Starter, Plan::Team, Plan::Business];
/// The plans one pays for (Stripe has a product for each).
pub const PAID: [Plan; 3] = [Plan::Starter, Plan::Team, Plan::Business];

impl Plan {
    pub const fn limits(self) -> &'static Limits {
        match self {
            Self::Free => &FREE,
            Self::Starter => &STARTER,
            Self::Team => &TEAM,
            Self::Business => &BUSINESS,
        }
    }

    /// As people read it: "Starter".
    pub const fn name(self) -> &'static str {
        match self {
            Self::Free => "Free",
            Self::Starter => "Starter",
            Self::Team => "Team",
            Self::Business => "Business",
        }
    }

    /// `starter`: its Stripe product is `romulus_<slug>`, its prices
    /// `<slug>_monthly` and `<slug>_yearly`.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Starter => "starter",
            Self::Team => "team",
            Self::Business => "business",
        }
    }

    /// Euro cents per `interval`; `None` for Free (nothing to pay).
    pub fn price_cents(self, interval: Interval) -> Option<u32> {
        let monthly = self.limits().monthly_cents;
        (monthly > 0).then_some(match interval {
            Interval::Month => monthly,
            Interval::Year => monthly * YEARLY_MONTHS,
        })
    }

    /// The Stripe price's `lookup_key`: `starter_monthly`, `team_yearly`…
    pub fn lookup_key(self, interval: Interval) -> Option<String> {
        self.price_cents(interval)?;
        Some(format!("{}_{}", self.slug(), interval_word(interval)))
    }

    /// The plan and interval a price's `lookup_key` names (an offer's price
    /// is its plan's: `team_founding_yearly` is Team, yearly).
    pub fn from_lookup_key(key: &str) -> Option<(Self, Interval)> {
        if let Some((offer, interval)) = Offer::from_lookup_key(key) {
            return Some((offer.plan, interval));
        }
        let (slug, interval) = key.rsplit_once('_')?;
        let interval = match interval {
            "monthly" => Interval::Month,
            "yearly" => Interval::Year,
            _ => return None,
        };
        let plan = PAID.into_iter().find(|plan| plan.slug() == slug)?;
        Some((plan, interval))
    }

    /// The Stripe product id the setup creates for a paid plan.
    pub fn product_id(self) -> Option<String> {
        (self != Self::Free).then(|| format!("romulus_{}", self.slug()))
    }
}

/// A price below a plan's own, on the plan's product, for as long as the
/// subscription stays on it, for the first [`Offer::teams`] organizations:
/// its own Stripe prices (`team_founding_monthly`, `team_founding_yearly`,
/// metadata `founding: 1`), which `romulus-api stripe-setup --founding-offer`
/// makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offer {
    /// `founding`: the `?offer=` the homepage's link carries, and the API's.
    pub slug: &'static str,
    pub plan: Plan,
    /// Euro cents a month instead of the plan's; a year costs
    /// [`YEARLY_MONTHS`] of them, as the plan's own yearly price.
    pub monthly_cents: u32,
    /// How many organizations get it, every interval together.
    pub teams: u32,
}

/// The founding-team offer: Team at €29 a month (€290 a year) instead of
/// €49 while the subscription stays on Team (Terms 7.11), for the first 20
/// teams, once per organization. On only with `ROMULUS_FOUNDING_OFFER`.
pub const FOUNDING: Offer = Offer {
    slug: "founding",
    plan: Plan::Team,
    monthly_cents: 2_900,
    teams: 20,
};

impl Offer {
    /// Euro cents per `interval` with the offer.
    pub const fn price_cents(self, interval: Interval) -> u32 {
        match interval {
            Interval::Month => self.monthly_cents,
            Interval::Year => self.monthly_cents * YEARLY_MONTHS,
        }
    }

    /// Its Stripe price's `lookup_key`: `team_founding_monthly`,
    /// `team_founding_yearly`.
    pub fn lookup_key(self, interval: Interval) -> String {
        format!(
            "{}_{}_{}",
            self.plan.slug(),
            self.slug,
            interval_word(interval)
        )
    }

    /// The offer and interval a price's `lookup_key` names.
    pub fn from_lookup_key(key: &str) -> Option<(Self, Interval)> {
        [Interval::Month, Interval::Year]
            .into_iter()
            .find(|interval| FOUNDING.lookup_key(*interval) == key)
            .map(|interval| (FOUNDING, interval))
    }
}

/// What a price's `lookup_key` costs, in euro cents per its interval: a
/// plan's price or an offer's.
pub fn lookup_cents(key: &str) -> Option<u32> {
    if let Some((offer, interval)) = Offer::from_lookup_key(key) {
        return Some(offer.price_cents(interval));
    }
    let (plan, interval) = Plan::from_lookup_key(key)?;
    plan.price_cents(interval)
}

const fn interval_word(interval: Interval) -> &'static str {
    match interval {
        Interval::Month => "monthly",
        Interval::Year => "yearly",
    }
}

/// The cheapest plan above `current` whose limits pass `allows`: the one an
/// upgrade prompt names. `None` when none does.
pub fn lifting(current: Plan, allows: impl Fn(&Limits) -> bool) -> Option<Plan> {
    ALL.into_iter()
        .filter(|plan| *plan > current)
        .find(|plan| allows(plan.limits()))
}

/// Whether `limit` (None: unlimited) leaves room for one more beyond `used`.
pub fn room(limit: Option<u32>, used: i64) -> bool {
    limit.is_none_or(|limit| used < i64::from(limit))
}

/// Whether `size` is within `largest`.
pub fn size_allowed(largest: PlaygroundSize, size: PlaygroundSize) -> bool {
    match largest {
        PlaygroundSize::Medium => true,
        PlaygroundSize::Small => size == PlaygroundSize::Small,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_founding_offer_is_team_at_29_a_month_for_20_teams() {
        assert_eq!(FOUNDING.plan, Plan::Team);
        assert_eq!(FOUNDING.teams, 20);
        assert_eq!(FOUNDING.price_cents(Interval::Month), 2_900);
        assert_eq!(FOUNDING.price_cents(Interval::Year), 29_000);
        assert_eq!(
            FOUNDING.lookup_key(Interval::Month),
            "team_founding_monthly"
        );
        assert_eq!(FOUNDING.lookup_key(Interval::Year), "team_founding_yearly");
        // Its prices are Team's, at its own amounts.
        assert_eq!(
            Plan::from_lookup_key("team_founding_yearly"),
            Some((Plan::Team, Interval::Year))
        );
        assert_eq!(
            Offer::from_lookup_key("team_founding_monthly"),
            Some((FOUNDING, Interval::Month))
        );
        assert_eq!(Offer::from_lookup_key("team_monthly"), None);
        assert_eq!(lookup_cents("team_founding_yearly"), Some(29_000));
        assert_eq!(lookup_cents("team_yearly"), Some(49_000));
        assert_eq!(lookup_cents("gold_founding_monthly"), None);
    }

    #[test]
    fn plans_are_ordered_and_each_lifts_what_the_one_before_caps() {
        for pair in ALL.windows(2) {
            let (lower, upper) = (pair[0].limits(), pair[1].limits());
            assert!(pair[0] < pair[1]);
            assert!(upper.monthly_cents > lower.monthly_cents);
            assert!(upper.databases > lower.databases);
            assert!(upper.schema_objects > lower.schema_objects);
            assert!(upper.playground_hours > lower.playground_hours);
            assert!(upper.approvals >= lower.approvals);
            assert!(
                upper.versions_per_database.unwrap_or(u32::MAX)
                    >= lower.versions_per_database.unwrap_or(u32::MAX)
            );
        }
    }

    #[test]
    fn the_draft_tiers_are_the_table() {
        let free = Plan::Free.limits();
        assert_eq!(
            (free.members, free.databases, free.versions_per_database),
            (Some(1), 1, Some(50))
        );
        assert_eq!((free.schema_objects, free.history_days), (50, Some(90)));
        assert_eq!(
            (free.playground_size, free.playground_hours, free.approvals),
            (PlaygroundSize::Small, 2, 0)
        );
        let starter = Plan::Starter.limits();
        assert_eq!(
            (
                starter.members,
                starter.databases,
                starter.versions_per_database
            ),
            (None, 3, Some(500))
        );
        assert_eq!(
            (
                starter.schema_objects,
                starter.history_days,
                starter.playground_hours
            ),
            (200, Some(365), 20)
        );
        let team = Plan::Team.limits();
        assert_eq!(
            (
                team.databases,
                team.versions_per_database,
                team.history_days
            ),
            (10, None, None)
        );
        assert_eq!(
            (team.playground_size, team.playground_hours, team.approvals),
            (PlaygroundSize::Medium, 80, 10)
        );
        assert!(!team.approvals_override);
        let business = Plan::Business.limits();
        assert_eq!((business.databases, business.schema_objects), (50, 5_000));
        assert!(business.approvals_override);
        assert!(
            ALL.iter()
                .skip(1)
                .all(|plan| plan.limits().members.is_none())
        );
    }

    #[test]
    fn a_year_costs_ten_months_and_free_costs_nothing() {
        assert_eq!(Plan::Starter.price_cents(Interval::Month), Some(1_900));
        assert_eq!(Plan::Starter.price_cents(Interval::Year), Some(19_000));
        assert_eq!(Plan::Team.price_cents(Interval::Year), Some(49_000));
        assert_eq!(Plan::Business.price_cents(Interval::Month), Some(14_900));
        assert_eq!(Plan::Free.price_cents(Interval::Month), None);
        assert_eq!(Plan::Free.lookup_key(Interval::Year), None);
    }

    #[test]
    fn lookup_keys_round_trip() {
        for plan in PAID {
            for interval in [Interval::Month, Interval::Year] {
                let key = plan.lookup_key(interval).unwrap();
                assert_eq!(Plan::from_lookup_key(&key), Some((plan, interval)));
            }
        }
        assert_eq!(
            Plan::Starter.lookup_key(Interval::Month).unwrap(),
            "starter_monthly"
        );
        assert_eq!(
            Plan::Team.lookup_key(Interval::Year).unwrap(),
            "team_yearly"
        );
        for bad in ["free_monthly", "starter", "starter_weekly", "", "_monthly"] {
            assert_eq!(Plan::from_lookup_key(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_cheapest_plan_that_lifts_a_cap_is_named() {
        assert_eq!(
            lifting(Plan::Free, |l| l.databases >= 2),
            Some(Plan::Starter)
        );
        assert_eq!(lifting(Plan::Free, |l| l.databases >= 4), Some(Plan::Team));
        assert_eq!(
            lifting(Plan::Starter, |l| l.approvals_override),
            Some(Plan::Business)
        );
        assert_eq!(
            lifting(Plan::Free, |l| size_allowed(
                l.playground_size,
                PlaygroundSize::Medium
            )),
            Some(Plan::Team)
        );
        assert_eq!(lifting(Plan::Business, |l| l.databases > 50), None);
        assert_eq!(
            lifting(Plan::Team, |l| l.databases >= 1),
            Some(Plan::Business)
        );
    }

    #[test]
    fn room_is_below_the_limit_and_unlimited_always_has_some() {
        assert!(room(Some(1), 0));
        assert!(!room(Some(1), 1));
        assert!(!room(Some(1), 5));
        assert!(room(None, i64::MAX));
        assert!(size_allowed(PlaygroundSize::Medium, PlaygroundSize::Small));
        assert!(!size_allowed(PlaygroundSize::Small, PlaygroundSize::Medium));
    }
}
