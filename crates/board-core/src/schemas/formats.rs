use jsonschema::paths::{LazyLocation, Location};
use jsonschema::{Keyword, ValidationError};
use serde_json::Value;

// The validator's custom-keyword factory requires its concrete error type.
#[allow(clippy::result_large_err)]
pub(super) fn validator(def: &Value) -> anyhow::Result<jsonschema::Validator> {
    Ok(jsonschema::draft202012::options()
        .should_validate_formats(true)
        .with_format("time", time)
        .with_format("date-time", |s: &str| {
            s.split_once(|c: char| matches!(c, 'T' | 't' | '\u{feff}') || c.is_whitespace())
                .is_some_and(|(d, t)| date(d) && time(t))
        })
        .with_format("hostname", |s: &str| {
            let name = s.strip_suffix('.').unwrap_or(s);
            name.len() <= 253
                && name
                    .split('.')
                    .all(|part| part.len() <= 63 && domain_label(part))
        })
        .with_format("email", email)
        .with_format("ipv4", |s: &str| {
            let mut parts = s.split('.');
            (0..4).all(|_| {
                parts
                    .next()
                    .is_some_and(|p| p.len() <= 3 && digits(p) && p.parse::<u8>().is_ok())
            }) && parts.next().is_none()
        })
        .with_format("uuid", |s: &str| {
            let raw = if s
                .get(..9)
                .is_some_and(|p| p.eq_ignore_ascii_case("urn:uuid:"))
            {
                &s[9..]
            } else {
                s
            };
            raw.len() == 36
                && raw.bytes().enumerate().all(|(i, b)| {
                    if matches!(i, 8 | 13 | 18 | 23) {
                        b == b'-'
                    } else {
                        b.is_ascii_hexdigit()
                    }
                })
        })
        .with_format("uri", |s: &str| {
            s.split_once(':')
                .is_some_and(|(_, path)| !path.is_empty() && !path.starts_with(['?', '#']))
                && fluent_uri::Uri::parse(s).is_ok()
        })
        .with_keyword("multipleOf", |_, n, loc| {
            let step = n.as_f64().filter(|f| *f > 0.0).ok_or_else(|| {
                ValidationError::custom(
                    loc.clone(),
                    Location::new(),
                    n,
                    "expected a positive number",
                )
            })?;
            Ok(Box::new(Step(step, loc)))
        })
        .build(def)?)
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn date(s: &str) -> bool {
    s.is_ascii()
        && s.len() == 10
        && s.as_bytes()[4] == b'-'
        && s.as_bytes()[7] == b'-'
        && digits(&s[..4])
        && digits(&s[5..7])
        && digits(&s[8..])
        && chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

fn time(s: &str) -> bool {
    if !s.is_ascii() || s.len() < 8 || s.as_bytes()[2] != b':' || s.as_bytes()[5] != b':' {
        return false;
    }
    let (h, m, sec) = (&s[..2], &s[3..5], &s[6..8]);
    if !(digits(h)
        && digits(m)
        && digits(sec)
        && h <= "23"
        && m <= "59"
        && (sec <= "59" || (h, m, sec) == ("23", "59", "60")))
    {
        return false;
    }
    let mut suffix = &s[8..];
    if let Some(rest) = suffix.strip_prefix('.') {
        let split = rest.bytes().take_while(u8::is_ascii_digit).count();
        if split == 0 {
            return false;
        }
        suffix = &rest[split..];
    }
    suffix.is_empty()
        || suffix.eq_ignore_ascii_case("z")
        || suffix.strip_prefix(['+', '-']).is_some_and(|zone| {
            // Offsets are syntactic here; optional minutes need not have a colon.
            (matches!(zone.len(), 2 | 4) && digits(zone))
                || (zone.len() == 5
                    && digits(&zone[..2])
                    && &zone[2..3] == ":"
                    && digits(&zone[3..]))
        })
}

fn domain_label(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && !s.ends_with('-')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    domain.contains('.')
        && domain.split('.').all(domain_label)
        && local.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+/=?^_`{|}~-".contains(&b))
        })
}

// Board's numeric data contract checks integrality of the binary64 quotient.
struct Step(f64, Location);
impl Keyword for Step {
    fn is_valid(&self, input: &Value) -> bool {
        input.as_f64().is_none_or(|x| (x / self.0).fract() == 0.0)
    }
    fn validate<'i>(&self, input: &'i Value, at: &LazyLocation) -> Result<(), ValidationError<'i>> {
        if self.is_valid(input) {
            Ok(())
        } else {
            Err(ValidationError::custom(
                self.1.clone(),
                at.into(),
                input,
                format!("not a multiple of {}", self.0),
            ))
        }
    }
}
