//! Column-name heuristics (docs/playground.md, "Realistic values"): a
//! column's name, its table's name and its type pick what its values look
//! like. A heuristic only applies to a type it fits (`email` on text, `*_at`
//! on a timestamp), and its values still go through the column's bounds.

use rand::RngExt;
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

use crate::{
    rng::{normal, pick},
    types::{Base, Type},
    value::{DAY, V},
    words,
};

/// What a column holds, by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Heuristic {
    Email,
    PersonName,
    FirstName,
    LastName,
    Username,
    CompanyName,
    ProductName,
    Department,
    JobTitle,
    Label,
    Phone,
    Street,
    AddressLine2,
    City,
    Region,
    Country,
    Zip,
    Currency,
    Url,
    ImageUrl,
    Domain,
    Slug,
    Ip,
    UserAgent,
    Token,
    Hash,
    PasswordHash,
    Locale,
    Timezone,
    Title,
    Paragraph,
    Sentence,
    Amount,
    Salary,
    Quantity,
    Latitude,
    Longitude,
    BirthDate,
    Age,
    Rating,
    Percent,
    Year,
    Position,
    Color,
    HexColor,
    Sku,
    Code,
    Status,
    Role,
    Plan,
    Kind,
    Source,
    EventKind,
    Tags,
    /// A past instant within the last two years (`created_at`).
    Past,
    /// Recent (`last_seen_at`, `updated_at`).
    Recent,
    /// Ahead of the anchor (`expires_at`, `due_on`).
    Future,
    /// Mostly NULL (`deleted_at`, `archived_at`).
    Rare,
    /// A boolean mostly true (`active`, `enabled`).
    MostlyTrue,
    /// A boolean mostly false (`deleted`, `archived`, `is_admin`).
    MostlyFalse,
    Settings,
    Metadata,
}

impl Heuristic {
    /// The values that make sense for it: the edge cases' extremes stay
    /// inside (a latitude of 1e300 breaks `ll_to_earth`, not a migration).
    pub fn range(self) -> Option<(f64, f64)> {
        match self {
            Heuristic::Latitude => Some((-90.0, 90.0)),
            Heuristic::Longitude => Some((-180.0, 180.0)),
            Heuristic::Age => Some((0.0, 120.0)),
            Heuristic::Rating => Some((1.0, 5.0)),
            Heuristic::Percent => Some((0.0, 100.0)),
            Heuristic::Year => Some((1900.0, 2100.0)),
            _ => None,
        }
    }

    /// The share of NULLs it implies for a nullable column, when not the
    /// usual one.
    pub fn null_rate(self) -> Option<f64> {
        match self {
            Heuristic::Rare => Some(0.9),
            Heuristic::AddressLine2 => Some(0.6),
            _ => None,
        }
    }
}

/// Splits `createdAt`, `created_at`, `Created At` into lower-case words.
pub fn words_of(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = name.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let boundary = c.is_uppercase()
            && i > 0
            && (chars[i - 1].is_lowercase()
                || chars.get(i + 1).is_some_and(|n| n.is_lowercase())
                    && chars[i - 1].is_uppercase());
        if boundary && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn singular(word: &str) -> String {
    if let Some(stem) = word.strip_suffix("ies") {
        format!("{stem}y")
    } else if word.ends_with("sses") || word.ends_with("xes") {
        word[..word.len() - 2].to_owned()
    } else if let Some(stem) = word.strip_suffix('s')
        && !word.ends_with("ss")
    {
        stem.to_owned()
    } else {
        word.to_owned()
    }
}

/// What kind of thing a table holds, for `name` columns.
fn table_subject(table: &str) -> &'static str {
    let words: Vec<String> = words_of(table).iter().map(|w| singular(w)).collect();
    let has = |list: &[&str]| words.iter().any(|w| list.contains(&w.as_str()));
    if has(&[
        "user", "customer", "person", "people", "employee", "staff", "member", "contact", "author",
        "profile", "student", "patient", "client", "account", "driver", "owner", "teacher",
        "agent",
    ]) {
        "person"
    } else if has(&[
        "organization",
        "organisation",
        "org",
        "company",
        "companie",
        "vendor",
        "supplier",
        "tenant",
        "partner",
        "merchant",
        "brand",
        "team",
    ]) {
        "company"
    } else if has(&[
        "product", "item", "sku", "good", "article", "listing", "offer",
    ]) {
        "product"
    } else if has(&["department", "division"]) {
        "department"
    } else if has(&[
        "city",
        "place",
        "location",
        "venue",
        "store",
        "branch",
        "office",
        "site",
        "warehouse",
    ]) {
        "place"
    } else {
        "thing"
    }
}

/// The heuristic for a column, if one fits its name and type.
#[allow(clippy::too_many_lines)]
pub fn choose(table: &str, column: &str, ty: &Type) -> Option<Heuristic> {
    use Heuristic as H;
    let w = words_of(column);
    let last = w.last().map_or("", String::as_str);
    let first = w.first().map_or("", String::as_str);
    let has = |x: &str| w.iter().any(|y| y == x);
    let text = ty.is_text();
    let base = &ty.base;
    let array = ty.array;
    let temporal = ty.is_temporal();
    let numeric = ty.is_numeric();
    let float = !array && matches!(base, Base::Float4 | Base::Float8 | Base::Numeric { .. });
    let integer = ty.is_integer();
    let boolean = !array && *base == Base::Bool;
    let json = !array && matches!(base, Base::Json | Base::Jsonb);

    if boolean {
        if has("deleted")
            || has("archived")
            || has("banned")
            || has("disabled")
            || has("locked")
            || has("admin")
            || has("blocked")
            || has("suspended")
            || has("test")
            || has("hidden")
        {
            return Some(H::MostlyFalse);
        }
        if has("active")
            || has("enabled")
            || has("visible")
            || has("verified")
            || has("confirmed")
            || has("published")
            || has("available")
            || has("valid")
            || last == "ok"
        {
            return Some(H::MostlyTrue);
        }
        return None;
    }
    if temporal {
        if has("deleted")
            || has("archived")
            || has("cancelled")
            || has("canceled")
            || has("revoked")
            || has("banned")
            || has("removed")
            || has("suspended")
            || has("failed")
        {
            return Some(H::Rare);
        }
        if has("birth") || has("dob") || has("birthday") || has("birthdate") {
            return Some(H::BirthDate);
        }
        if has("expires")
            || has("expiry")
            || has("expiration")
            || has("due")
            || has("until")
            || has("deadline")
            || has("scheduled")
            || has("ends")
            || has("end")
        {
            return Some(H::Future);
        }
        if has("updated")
            || has("modified")
            || has("seen")
            || has("last")
            || has("accessed")
            || has("refreshed")
        {
            return Some(H::Recent);
        }
        return Some(H::Past);
    }
    if json {
        if has("settings") || has("preferences") || has("prefs") || has("config") || has("options")
        {
            return Some(H::Settings);
        }
        return Some(H::Metadata);
    }
    if array
        && ty.base != Base::Bool
        && (has("tags") || has("labels") || has("keywords") || has("categories"))
    {
        return Some(H::Tags);
    }
    if array {
        return None;
    }
    if float && (has("lat") || has("latitude")) {
        return Some(H::Latitude);
    }
    if float && (has("lng") || has("lon") || has("long") || has("longitude")) {
        return Some(H::Longitude);
    }
    if numeric {
        if has("salary") || has("wage") || has("income") || has("compensation") {
            return Some(H::Salary);
        }
        if has("price")
            || has("amount")
            || has("total")
            || has("subtotal")
            || has("cost")
            || has("balance")
            || has("fee")
            || has("revenue")
            || has("tax")
            || has("discount")
            || has("budget")
            || has("cents")
            || has("paid")
            || has("spent")
        {
            return Some(H::Amount);
        }
        if has("quantity")
            || has("qty")
            || has("stock")
            || has("count")
            || has("inventory")
            || has("seats")
        {
            return Some(H::Quantity);
        }
        if has("age") {
            return Some(H::Age);
        }
        if has("rating") || has("stars") || has("score") {
            return Some(H::Rating);
        }
        if has("percent") || has("percentage") || has("pct") || has("ratio") || last == "rate" {
            return Some(H::Percent);
        }
        if integer && (has("year") || last == "yr") {
            return Some(H::Year);
        }
        if integer
            && (has("position")
                || has("ordinal")
                || has("rank")
                || has("sort")
                || has("order")
                || has("seq")
                || has("index")
                || has("priority")
                || has("step")
                || has("level"))
        {
            return Some(H::Position);
        }
        return None;
    }
    if !text {
        return None;
    }
    let max = ty.max_chars();
    let fits = |n: usize| max.is_none_or(|m| m >= n);
    if has("email") || has("mail") {
        return fits(6).then_some(H::Email);
    }
    if has("password") || has("passwd") || (has("pass") && has("hash")) {
        return fits(20).then_some(H::PasswordHash);
    }
    if last == "hash"
        || last == "digest"
        || last == "checksum"
        || last == "sha"
        || last == "sha256"
        || last == "fingerprint"
    {
        return Some(H::Hash);
    }
    if has("token")
        || has("secret")
        || last == "key" && (has("api") || has("access") || has("secret"))
        || has("nonce")
    {
        return Some(H::Token);
    }
    if has("url") || has("uri") || has("link") || has("website") || has("homepage") || has("href") {
        if has("avatar")
            || has("image")
            || has("photo")
            || has("picture")
            || has("logo")
            || has("thumbnail")
            || has("icon")
        {
            return fits(20).then_some(H::ImageUrl);
        }
        return fits(20).then_some(H::Url);
    }
    if has("domain") || has("hostname") || last == "host" {
        return fits(8).then_some(H::Domain);
    }
    if has("slug") || has("handle") || has("permalink") {
        return Some(H::Slug);
    }
    if has("ip") || last == "ipv4" || last == "ipv6" {
        return fits(7).then_some(H::Ip);
    }
    if has("agent") && (has("user") || has("browser")) || last == "ua" {
        return Some(H::UserAgent);
    }
    if has("locale") || has("lang") || has("language") {
        return fits(2).then_some(H::Locale);
    }
    if has("timezone") || has("tz") || (has("time") && has("zone")) {
        return Some(H::Timezone);
    }
    if has("currency") || (has("currency") && has("code")) {
        return fits(3).then_some(H::Currency);
    }
    if has("country") {
        return Some(H::Country);
    }
    if has("phone")
        || has("mobile")
        || has("tel")
        || has("telephone")
        || has("fax")
        || has("msisdn")
    {
        return fits(8).then_some(H::Phone);
    }
    if has("zip") || has("postcode") || has("postal") || (has("post") && has("code")) {
        return fits(4).then_some(H::Zip);
    }
    if has("city") || has("town") {
        return Some(H::City);
    }
    if has("state") && !has("status") && fits(2) && max.is_some_and(|m| m <= 3)
        || has("province")
        || has("county")
        || first == "region"
    {
        return Some(H::Region);
    }
    if (has("line2") || has("address2") || has("apartment") || has("suite") || has("unit"))
        && !has("price")
    {
        return Some(H::AddressLine2);
    }
    if has("street") || has("address") || has("line1") || has("address1") {
        return Some(H::Street);
    }
    if has("username")
        || has("login")
        || has("nickname")
        || has("nick")
        || has("screen")
        || last == "user"
    {
        return Some(H::Username);
    }
    if has("first") && has("name") || has("firstname") || has("given") || has("forename") {
        return Some(H::FirstName);
    }
    if has("last") && has("name") || has("lastname") || has("surname") || has("family") {
        return Some(H::LastName);
    }
    if has("company")
        || has("organization")
        || has("organisation")
        || has("employer")
        || has("vendor")
        || has("supplier")
        || has("brand")
    {
        return Some(H::CompanyName);
    }
    if has("department") || has("dept") || has("team") {
        return Some(H::Department);
    }
    if has("job") || has("occupation") || (has("title") && has("job")) {
        return Some(H::JobTitle);
    }
    if has("sku") {
        return Some(H::Sku);
    }
    if has("color") || has("colour") {
        return Some(if max.is_some_and(|m| m == 7) {
            H::HexColor
        } else {
            H::Color
        });
    }
    if has("status") || has("state") {
        return Some(H::Status);
    }
    if has("role") {
        return Some(H::Role);
    }
    if has("plan") || has("tier") || has("subscription") {
        return Some(H::Plan);
    }
    if has("source") || has("channel") || has("origin") || has("referrer") {
        return Some(H::Source);
    }
    if (has("kind") || has("type") || has("event") || has("action"))
        && table_subject(table) == "thing"
        && words_of(table).iter().any(|t| {
            t.starts_with("event")
                || t.starts_with("log")
                || t.starts_with("audit")
                || t.starts_with("activit")
        })
    {
        return Some(H::EventKind);
    }
    if has("kind") || has("type") || has("category") || has("variant") || has("mode") {
        return Some(H::Kind);
    }
    if has("code") || has("ref") || has("reference") || last == "number" || has("serial") {
        return Some(H::Code);
    }
    if has("description")
        || has("body")
        || has("content")
        || has("bio")
        || has("about")
        || has("summary")
        || has("text")
        || has("details")
        || has("abstract")
        || has("message")
    {
        return Some(if fits(200) { H::Paragraph } else { H::Sentence });
    }
    if has("note")
        || has("notes")
        || has("comment")
        || has("comments")
        || has("remark")
        || has("reason")
        || has("caption")
        || has("subject")
        || has("headline")
        || has("excerpt")
    {
        return Some(H::Sentence);
    }
    if has("title") || has("heading") || has("label") || has("caption") {
        return Some(match table_subject(table) {
            "product" => H::ProductName,
            _ => H::Title,
        });
    }
    if has("name") || last == "fullname" {
        if has("full") || has("display") || has("contact") || has("person") {
            return Some(H::PersonName);
        }
        return Some(match table_subject(table) {
            "person" => H::PersonName,
            "company" => H::CompanyName,
            "product" => H::ProductName,
            "department" => H::Department,
            "place" => H::City,
            _ => H::Label,
        });
    }
    if has("label") || has("tag") {
        return Some(H::Label);
    }
    None
}

/// Generation context for one value.
pub struct Ctx {
    pub row: u64,
    /// "now": microseconds since the epoch.
    pub anchor: i64,
}

fn person(rng: &mut ChaCha8Rng) -> (&'static str, &'static str) {
    (pick(rng, words::FIRST_NAMES), pick(rng, words::LAST_NAMES))
}

fn ascii_lower(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => Some('a'),
            'é' | 'è' | 'ê' | 'ë' => Some('e'),
            'í' | 'ì' | 'î' | 'ï' => Some('i'),
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' => Some('o'),
            'ú' | 'ù' | 'û' | 'ü' => Some('u'),
            'ç' => Some('c'),
            'ñ' => Some('n'),
            c if c.is_ascii_alphanumeric() => Some(c.to_ascii_lowercase()),
            ' ' | '-' | '.' | '_' => Some('-'),
            _ => None,
        })
        .collect()
}

fn title_case(word: &str) -> String {
    let mut c = word.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

fn sentence(rng: &mut ChaCha8Rng, min: usize, max: usize) -> String {
    let n = rng.random_range(min..=max);
    let mut s = String::new();
    for i in 0..n {
        let w = pick(rng, words::WORDS);
        if i == 0 {
            s.push_str(&title_case(w));
        } else {
            s.push(' ');
            s.push_str(w);
        }
    }
    s.push('.');
    s
}

fn hex(rng: &mut ChaCha8Rng, n: usize) -> String {
    (0..n)
        .map(|_| char::from_digit(rng.random_range(0..16), 16).unwrap_or('0'))
        .collect()
}

fn token(rng: &mut ChaCha8Rng, n: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    (0..n)
        .map(|_| char::from(ALPHABET[rng.random_range(0..ALPHABET.len())]))
        .collect()
}

/// A positive, log-normal amount (most small, a long tail).
fn amount(rng: &mut ChaCha8Rng) -> f64 {
    libm::exp(3.5 + 1.2 * normal(rng)).clamp(0.5, 250_000.0)
}

/// The value it gives; `None` when the type turns out not to fit.
#[allow(clippy::too_many_lines)]
pub fn generate(h: Heuristic, table: &str, ty: &Type, rng: &mut ChaCha8Rng, cx: &Ctx) -> Option<V> {
    use Heuristic as H;
    let max = ty.max_chars();
    let text = |s: String| Some(V::Text(s));
    match h {
        H::Email => {
            let (f, l) = person(rng);
            let domain = pick(rng, words::EMAIL_DOMAINS);
            let local = match rng.random_range(0..4) {
                0 => format!("{}.{}", ascii_lower(f), ascii_lower(l)),
                1 => format!("{}{}", ascii_lower(&f[..1]), ascii_lower(l)),
                2 => ascii_lower(f),
                _ => format!("{}_{}", ascii_lower(f), ascii_lower(l)),
            };
            text(format!("{local}@{domain}"))
        }
        H::PersonName => {
            let (f, l) = person(rng);
            text(format!("{f} {l}"))
        }
        H::FirstName => text(person(rng).0.to_owned()),
        H::LastName => text(person(rng).1.to_owned()),
        H::Username => {
            let (f, l) = person(rng);
            text(match rng.random_range(0..3) {
                0 => ascii_lower(f),
                1 => format!("{}_{}", ascii_lower(f), ascii_lower(&l[..1])),
                _ => format!("{}{}", ascii_lower(&f[..1]), ascii_lower(l)),
            })
        }
        H::CompanyName => text(format!(
            "{} {}",
            pick(rng, words::COMPANY_WORDS),
            pick(rng, words::COMPANY_SUFFIXES)
        )),
        H::ProductName => text(format!(
            "{} {}",
            pick(rng, words::PRODUCT_ADJECTIVES),
            pick(rng, words::PRODUCT_NOUNS)
        )),
        H::Department => text(pick(rng, words::DEPARTMENTS).to_string()),
        H::JobTitle => text(pick(rng, words::JOB_TITLES).to_string()),
        H::Label => {
            let a = pick(rng, words::WORDS);
            let b = pick(rng, words::WORDS);
            text(format!("{} {b}", title_case(a)))
        }
        H::Phone => text(format!(
            "+1-{}-555-01{:02}",
            rng.random_range(201..990),
            rng.random_range(0..100)
        )),
        H::Street => text(format!(
            "{} {} {}",
            rng.random_range(1..400),
            pick(rng, words::STREETS),
            pick(rng, words::STREET_KINDS)
        )),
        H::AddressLine2 => text(format!("Apt {}", rng.random_range(1..60))),
        H::City => text(pick(rng, words::CITIES).0.to_owned()),
        H::Region => {
            const REGIONS: &[&str] = &["CA", "NY", "TX", "WA", "IL", "MA", "ON", "QC", "NSW", "BY"];
            let r = pick(rng, REGIONS);
            text(match max {
                Some(m) if r.len() > m => r[..m].to_owned(),
                _ => (*r).to_owned(),
            })
        }
        H::Country => {
            let (a2, a3, name) = *pick(rng, words::COUNTRIES);
            text(match max {
                Some(2) => a2.to_owned(),
                Some(3) => a3.to_owned(),
                Some(m) if m < name.chars().count() => a2.to_owned(),
                _ if ty.base == Base::Char(2) => a2.to_owned(),
                _ => name.to_owned(),
            })
        }
        H::Zip => text(format!("{:05}", rng.random_range(1000..99_999))),
        H::Currency => text(pick(rng, words::CURRENCIES).to_string()),
        H::Url => {
            let path = pick(rng, words::WORDS);
            text(format!(
                "https://{}/{path}/{}",
                pick(rng, words::EMAIL_DOMAINS),
                cx.row + 1
            ))
        }
        H::ImageUrl => text(format!(
            "https://images.example.com/{}/{}.png",
            singular(&words_of(table).last().cloned().unwrap_or_default()),
            cx.row + 1
        )),
        H::Domain => text(format!(
            "{}.{}",
            ascii_lower(pick(rng, words::COMPANY_WORDS)),
            pick(rng, &["example.com", "example.org", "test"])
        )),
        H::Slug => text(format!(
            "{}-{}",
            pick(rng, words::WORDS),
            pick(rng, words::WORDS)
        )),
        H::Ip => text(match rng.random_range(0..8) {
            0 => format!("2001:db8::{:x}", rng.random_range(1..0xffff)),
            n => {
                let prefix = ["192.0.2", "198.51.100", "203.0.113"][n % 3];
                format!("{prefix}.{}", rng.random_range(1..255))
            }
        }),
        H::UserAgent => text(pick(rng, words::USER_AGENTS).to_string()),
        H::Token => text(token(rng, max.map_or(40, |m| m.min(40)))),
        H::Hash => text(hex(rng, max.map_or(64, |m| m.min(64)))),
        H::PasswordHash => text(format!(
            "$2b$12${}",
            token(rng, 53).replace('-', ".").replace('_', "/")
        )),
        H::Locale => {
            let l = pick(rng, words::LOCALES);
            text(match max {
                Some(m) if m < 5 => l[..2].to_owned(),
                _ => (*l).to_owned(),
            })
        }
        H::Timezone => text(pick(rng, words::TIMEZONES).to_string()),
        H::Title => {
            let s = sentence(rng, 2, 6);
            text(s.trim_end_matches('.').to_owned())
        }
        H::Paragraph => {
            let n = rng.random_range(1..4);
            let s = (0..n)
                .map(|_| sentence(rng, 6, 14))
                .collect::<Vec<_>>()
                .join(" ");
            text(s)
        }
        H::Sentence => text(sentence(rng, 4, 12)),
        H::Amount => Some(V::Float((amount(rng) * 100.0).round() / 100.0)),
        H::Salary => Some(V::Float(
            (rng.random_range(28_000.0..180_000.0f64) / 100.0).round() * 100.0,
        )),
        H::Quantity => Some(V::Int(i128::from(1 + crate::rng::skewed(rng, 20)))),
        H::Latitude => {
            let (_, _, lat, _) = *pick(rng, words::CITIES);
            Some(V::Float(((lat + normal(rng) * 0.05) * 1e5).round() / 1e5))
        }
        H::Longitude => {
            let (_, _, _, lon) = *pick(rng, words::CITIES);
            Some(V::Float(((lon + normal(rng) * 0.05) * 1e5).round() / 1e5))
        }
        H::BirthDate => {
            let years = rng.random_range(18.0..85.0);
            #[allow(clippy::cast_possible_truncation)]
            let days = (years * 365.25) as i64;
            Some(V::Date(cx.anchor.div_euclid(DAY) - days))
        }
        H::Age => Some(V::Int(rng.random_range(18..90))),
        H::Rating => Some(V::Int(match rng.random_range(0..10) {
            0 => 1,
            1 => 2,
            2 | 3 => 3,
            4..=6 => 4,
            _ => 5,
        })),
        H::Percent => Some(V::Float(
            (rng.random_range(0.0..100.0f64) * 100.0).round() / 100.0,
        )),
        H::Year => Some(V::Int(rng.random_range(1990..=2026))),
        H::Position => Some(V::Int(i128::from(crate::rng::skewed(rng, 50)))),
        H::Color => text(pick(rng, words::COLORS).to_string()),
        H::HexColor => text(format!("#{}", hex(rng, 6))),
        H::Sku => text(format!(
            "{}-{:04}",
            words::PRODUCT_NOUNS[rng.random_range(0..words::PRODUCT_NOUNS.len())]
                .chars()
                .filter(char::is_ascii_alphabetic)
                .take(3)
                .collect::<String>()
                .to_uppercase(),
            rng.random_range(0..10_000)
        )),
        H::Code => {
            const ALNUM: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
            let n = max.map_or(8, |m| m.clamp(1, 8));
            text(
                (0..n)
                    .map(|_| char::from(ALNUM[rng.random_range(0..ALNUM.len())]))
                    .collect(),
            )
        }
        H::Status => text(weighted(rng, words::STATUSES).to_owned()),
        H::Role => text(weighted(rng, words::ROLES).to_owned()),
        H::Plan => text(weighted(rng, words::PLANS).to_owned()),
        H::Kind => text(weighted(rng, words::KINDS).to_owned()),
        H::Source => text(weighted(rng, words::SOURCES).to_owned()),
        H::EventKind => text(weighted(rng, words::EVENT_KINDS).to_owned()),
        H::Tags => {
            let n = rng.random_range(0..4);
            let mut tags: Vec<V> = Vec::new();
            for _ in 0..n {
                let t = V::Text(pick(rng, words::TAGS).to_string());
                if !tags.contains(&t) {
                    tags.push(t);
                }
            }
            Some(V::Array(tags))
        }
        H::Past => {
            let back = rng.random_range(0..730 * DAY);
            Some(V::Ts(cx.anchor - back))
        }
        H::Recent => {
            let back = rng.random_range(0..30 * DAY);
            Some(V::Ts(cx.anchor - back))
        }
        H::Future => {
            let ahead = rng.random_range(DAY..365 * DAY);
            Some(V::Ts(cx.anchor + ahead))
        }
        H::Rare => {
            let back = rng.random_range(0..365 * DAY);
            Some(V::Ts(cx.anchor - back))
        }
        H::MostlyTrue => Some(V::Bool(rng.random_bool(0.85))),
        H::MostlyFalse => Some(V::Bool(rng.random_bool(0.1))),
        H::Settings => {
            let theme = pick(rng, &["light", "dark", "system"]);
            let notify = rng.random_bool(0.7);
            let lang = &pick(rng, words::LOCALES)[..2];
            text(format!(
                r#"{{"theme": "{theme}", "notifications": {notify}, "language": "{lang}"}}"#
            ))
        }
        H::Metadata => {
            let source = pick(rng, words::SOURCES);
            let version = rng.random_range(1..6);
            let tag = pick(rng, words::TAGS);
            text(format!(
                r#"{{"source": "{source}", "version": {version}, "tags": ["{tag}"]}}"#
            ))
        }
    }
}

/// The first items of a list come up more often (statuses: mostly active).
fn weighted<'a>(rng: &mut ChaCha8Rng, items: &[&'a str]) -> &'a str {
    #[allow(clippy::cast_possible_truncation)]
    let i = crate::rng::skewed(rng, items.len() as u64) as usize;
    items[i]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{rng, types::Base};

    fn ty(base: Base) -> Type {
        Type::scalar(base)
    }

    #[test]
    fn names_split_into_words() {
        assert_eq!(words_of("createdAt"), ["created", "at"]);
        assert_eq!(words_of("full_name"), ["full", "name"]);
        assert_eq!(words_of("zip code"), ["zip", "code"]);
        assert_eq!(words_of("HTTPStatus"), ["http", "status"]);
        assert_eq!(words_of("Email"), ["email"]);
    }

    #[test]
    fn heuristics_follow_names_and_types() {
        let text = ty(Base::Text);
        let ts = ty(Base::Timestamp {
            tz: true,
            precision: None,
        });
        assert_eq!(choose("users", "email", &text), Some(Heuristic::Email));
        assert_eq!(choose("users", "name", &text), Some(Heuristic::PersonName));
        assert_eq!(
            choose("organizations", "name", &text),
            Some(Heuristic::CompanyName)
        );
        assert_eq!(
            choose("products", "name", &text),
            Some(Heuristic::ProductName)
        );
        assert_eq!(choose("users", "created_at", &ts), Some(Heuristic::Past));
        assert_eq!(choose("users", "updated_at", &ts), Some(Heuristic::Recent));
        assert_eq!(choose("users", "deleted_at", &ts), Some(Heuristic::Rare));
        assert_eq!(
            choose("t", "country", &ty(Base::Char(2))),
            Some(Heuristic::Country)
        );
        assert_eq!(
            choose(
                "orders",
                "price",
                &ty(Base::Numeric {
                    precision: Some(10),
                    scale: 2
                })
            ),
            Some(Heuristic::Amount)
        );
        // A heuristic never applies to a type it does not fit.
        assert_eq!(choose("users", "email", &ty(Base::Int4)), None);
        assert_eq!(choose("t", "created_at", &text), None);
    }

    #[test]
    fn emails_use_reserved_domains() {
        let k = rng::key(1, &["users", "email"]);
        for row in 0..50 {
            let cx = Ctx { row, anchor: 0 };
            let Some(V::Text(e)) = generate(
                Heuristic::Email,
                "users",
                &ty(Base::Text),
                &mut rng::cell(&k, row, 0),
                &cx,
            ) else {
                panic!()
            };
            let domain = e.split('@').nth(1).unwrap();
            assert!(words::EMAIL_DOMAINS.contains(&domain), "{e}");
        }
    }

    #[test]
    fn a_two_letter_country_is_an_iso_code() {
        let k = rng::key(1, &["t", "country"]);
        let cx = Ctx { row: 0, anchor: 0 };
        let v = generate(
            Heuristic::Country,
            "t",
            &ty(Base::Char(2)),
            &mut rng::cell(&k, 0, 0),
            &cx,
        )
        .unwrap();
        assert_eq!(v.as_str().unwrap().len(), 2);
    }

    /// Every heuristic gives a value of its type, from reserved names where
    /// it names hosts.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn every_heuristic_gives_values_of_its_kind() {
        use Heuristic as H;
        let text = ty(Base::Text);
        let all = [
            H::Email,
            H::PersonName,
            H::FirstName,
            H::LastName,
            H::Username,
            H::CompanyName,
            H::ProductName,
            H::Department,
            H::JobTitle,
            H::Label,
            H::Phone,
            H::Street,
            H::AddressLine2,
            H::City,
            H::Region,
            H::Country,
            H::Zip,
            H::Currency,
            H::Url,
            H::ImageUrl,
            H::Domain,
            H::Slug,
            H::Ip,
            H::UserAgent,
            H::Token,
            H::Hash,
            H::PasswordHash,
            H::Locale,
            H::Timezone,
            H::Title,
            H::Paragraph,
            H::Sentence,
            H::Color,
            H::HexColor,
            H::Sku,
            H::Code,
            H::Status,
            H::Role,
            H::Plan,
            H::Kind,
            H::Source,
            H::EventKind,
        ];
        #[allow(clippy::case_sensitive_file_extension_comparisons)]
        let reserved = |host: &str| {
            host.ends_with("example.com")
                || host.ends_with("example.org")
                || host.ends_with("example.net")
                || host.ends_with(".test")
                || host.ends_with(".example")
                || host == "test"
        };
        for h in all {
            let k = rng::key(5, &["t", &format!("{h:?}")]);
            for row in 0..50 {
                let cx = Ctx {
                    row,
                    anchor: 1_790_000_000_000_000,
                };
                let v = generate(h, "things", &text, &mut rng::cell(&k, row, 0), &cx).unwrap();
                let s = v.as_str().unwrap();
                assert!(!s.is_empty(), "{h:?}");
                match h {
                    H::Email => assert!(reserved(s.split('@').nth(1).unwrap()), "{s}"),
                    H::Url | H::ImageUrl => {
                        let host = s.trim_start_matches("https://").split('/').next().unwrap();
                        assert!(reserved(host), "{s}");
                    }
                    H::Domain => assert!(reserved(s), "{s}"),
                    H::Phone => assert!(s.contains("-555-01"), "{s}"),
                    H::Currency => assert_eq!(s.len(), 3),
                    H::Slug => {
                        assert!(s.chars().all(|c| c.is_ascii_lowercase() || c == '-'), "{s}");
                    }
                    H::Hash => assert!(s.chars().all(|c| c.is_ascii_hexdigit()), "{s}"),
                    _ => {}
                }
            }
        }
        let numbers = [
            (H::Amount, 0.0, 250_000.0),
            (H::Salary, 28_000.0, 180_000.0),
            (H::Quantity, 1.0, 20.0),
            (H::Latitude, -90.0, 90.0),
            (H::Longitude, -180.0, 180.0),
            (H::Age, 18.0, 90.0),
            (H::Rating, 1.0, 5.0),
            (H::Percent, 0.0, 100.0),
            (H::Year, 1990.0, 2026.0),
            (H::Position, 0.0, 50.0),
        ];
        let float = ty(Base::Float8);
        for (h, lo, hi) in numbers {
            let k = rng::key(5, &["t", &format!("{h:?}")]);
            for row in 0..100 {
                let cx = Ctx { row, anchor: 0 };
                let v = generate(h, "t", &float, &mut rng::cell(&k, row, 0), &cx).unwrap();
                let f = v.as_f64().unwrap();
                assert!((lo..=hi).contains(&f), "{h:?}: {f}");
            }
        }
    }

    #[test]
    fn times_fall_where_their_names_say() {
        let ts = ty(Base::Timestamp {
            tz: true,
            precision: None,
        });
        let anchor = 1_790_000_000_000_000;
        for (h, past) in [
            (Heuristic::Past, true),
            (Heuristic::Recent, true),
            (Heuristic::Future, false),
        ] {
            let k = rng::key(5, &["t", &format!("{h:?}")]);
            for row in 0..100 {
                let cx = Ctx { row, anchor };
                let Some(V::Ts(t)) = generate(h, "t", &ts, &mut rng::cell(&k, row, 0), &cx) else {
                    panic!()
                };
                assert_eq!(t <= anchor, past, "{h:?}");
                assert!((t - anchor).abs() <= 730 * DAY);
            }
        }
    }
}
