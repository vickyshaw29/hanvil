//! The mirror node's wire vocabulary: `0.0.N` ids, `sec.nanos` timestamps, `links.next`, keys,
//! and the record-file name a block is known by. Every encoding here is cited to
//! `research/hiero-mirror-node/rest/api/v1/openapi.yml`; the field sets live with each handler.

use alloy_primitives::Address;
use serde_json::{Value, json};

use super::Error;
use crate::state::{EntityId, Key, Timestamp};

/// `openapi.yml:3774` Timestamp: `"1586567700.453054000"`.
pub fn timestamp(at: Timestamp) -> Value {
    json!(at.to_string())
}

/// `openapi.yml:3784` TimestampRange. `to` is exclusive and null while the range is open.
pub fn timestamp_range(from: Timestamp, to: Option<Timestamp>) -> Value {
    json!({
        "from": from.to_string(),
        "to": to.map_or(Value::Null, |t| json!(t.to_string())),
    })
}

/// `openapi.yml:2976` Links. Hanvil answers every list in one page.
pub fn links() -> Value {
    json!({ "next": Value::Null })
}

/// Hex with the `0x` prefix, as every binary field on the mirror carries it.
pub fn hex(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

/// Base64, RFC 4648 with padding — `format: byte` in the spec.
pub fn base64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// `openapi.yml:2965` Key: the raw public key in hex without a prefix, tagged by curve.
pub fn key(key: Option<&Key>) -> Value {
    match key {
        Some(Key::EcdsaSecp256k1(bytes)) => {
            json!({ "_type": "ECDSA_SECP256K1", "key": hex::encode(bytes) })
        }
        Some(Key::Ed25519(bytes)) => json!({ "_type": "ED25519", "key": hex::encode(bytes) }),
        None => Value::Null,
    }
}

/// `0.0.x-sss-nnn`, the only transaction-id form the mirror accepts in a URL
/// (`openapi.yml:5565`). The SDK's `0.0.x@sss.nnn` is rejected there with a 400.
pub fn transaction_id(payer: EntityId, valid_start: Timestamp) -> String {
    format!("{payer}-{}-{:09}", valid_start.secs, valid_start.nanos)
}

/// Parse `0.0.x-sss-nnn`. The SDK form is named in the error, because pasting it is the mistake
/// this endpoint sees most (`hedera-harness` PR #39, `mirrorNode.ts` `normalizeTransactionId`).
pub fn parse_transaction_id(text: &str) -> Result<(EntityId, Timestamp), Error> {
    let parts: Vec<&str> = text.split('-').collect();
    let [id, secs, nanos] = parts[..] else {
        return Err(Error::invalid_transaction_id());
    };
    let entity = parse_entity_id(id).map_err(|_| Error::invalid_transaction_id())?;
    Ok((
        entity,
        Timestamp {
            secs: secs.parse().map_err(|_| Error::invalid_transaction_id())?,
            nanos: nanos.parse().map_err(|_| Error::invalid_transaction_id())?,
        },
    ))
}

/// One `timestamp=` clause: an optional comparison operator and the instant it compares against
/// (`openapi.yml:5294` timestampQueryParam, pattern `^((eq|gt|gte|lt|lte|ne):)?\d{1,10}(\.\d{1,9})?$`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimestampFilter {
    operator: Operator,
    at: Timestamp,
}

/// The comparisons the mirror accepts on a timestamp. No operator means `eq`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operator {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
}

impl TimestampFilter {
    /// Whether a consensus timestamp satisfies this clause.
    pub fn matches(&self, at: Timestamp) -> bool {
        match self.operator {
            Operator::Eq => at == self.at,
            Operator::Ne => at != self.at,
            Operator::Gt => at > self.at,
            Operator::Gte => at >= self.at,
            Operator::Lt => at < self.at,
            Operator::Lte => at <= self.at,
        }
    }
}

/// Parse one `timestamp=` value. Seconds alone mean nanosecond zero, so `gte:5` starts at the
/// top of that second; a fractional part is left-aligned, so `.1` is 100,000,000 nanoseconds.
pub fn parse_timestamp_filter(text: &str) -> Result<TimestampFilter, Error> {
    let invalid = || Error::invalid_parameter("timestamp");
    let (operator, value) = match text.split_once(':') {
        Some(("eq", rest)) => (Operator::Eq, rest),
        Some(("ne", rest)) => (Operator::Ne, rest),
        Some(("gt", rest)) => (Operator::Gt, rest),
        Some(("gte", rest)) => (Operator::Gte, rest),
        Some(("lt", rest)) => (Operator::Lt, rest),
        Some(("lte", rest)) => (Operator::Lte, rest),
        Some(_) => return Err(invalid()),
        None => (Operator::Eq, text),
    };

    let (secs, nanos) = match value.split_once('.') {
        None => (value, "0"),
        Some((secs, nanos)) if !nanos.is_empty() && nanos.len() <= 9 => (secs, nanos),
        Some(_) => return Err(invalid()),
    };
    if secs.is_empty() || secs.len() > 10 || !secs.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    if !nanos.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    // `.1` is a tenth of a second, so the fraction is padded on the right, not the left.
    let padded = format!("{nanos:0<9}");
    Ok(TimestampFilter {
        operator,
        at: Timestamp {
            secs: secs.parse().map_err(|_| invalid())?,
            nanos: padded.parse().map_err(|_| invalid())?,
        },
    })
}

/// What a path segment that names an entity can be.
pub enum Reference {
    /// `0.0.N`, or a bare `N`.
    Entity(EntityId),
    /// A 20-byte EVM address: an account alias, a long-zero id, or a contract.
    Evm(Address),
}

/// Parse `{idOrAliasOrEvmAddress}`. Base32 key aliases are not minted by Hanvil and are refused
/// here rather than resolved to something else.
pub fn parse_reference(text: &str, parameter: &str) -> Result<Reference, Error> {
    if let Some(body) = text.strip_prefix("0x") {
        return match hex::decode(body) {
            Ok(bytes) if bytes.len() == 20 => Ok(Reference::Evm(Address::from_slice(&bytes))),
            _ => Err(Error::invalid_parameter(parameter)),
        };
    }
    parse_entity_id(text)
        .map(Reference::Entity)
        .map_err(|_| Error::invalid_parameter(parameter))
}

/// `0.0.N`, `0.N` or `N`, all in shard 0 realm 0 — anything else is not an entity Hanvil holds.
pub fn parse_entity_id(text: &str) -> Result<EntityId, ()> {
    let parts: Vec<&str> = text.split('.').collect();
    let num = match parts[..] {
        [num] => num,
        ["0", num] => num,
        ["0", "0", num] => num,
        _ => return Err(()),
    };
    num.parse().map(EntityId).map_err(|_| ())
}

/// The name a block is known by: the record file that closed it,
/// `2022-05-03T06_46_26.060890949Z.rcd` (`openapi.yml:3371`).
pub fn record_file_name(at: Timestamp) -> String {
    let (year, month, day) = civil_from_days((at.secs / 86_400) as i64);
    let seconds_of_day = at.secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}_{:02}_{:02}.{:09}Z.rcd",
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60,
        at.nanos,
    )
}

/// Civil date from a count of days since 1970-01-01. Howard Hinnant's `civil_from_days`, which is
/// exact for every day in the range a `u64` second count can reach. `chrono` is not a dependency.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_file_name_matches_the_spec_example() {
        // openapi.yml:3371 — "2022-05-03T06_46_26.060890949Z.rcd" at 1651560386.060890949.
        let at = Timestamp {
            secs: 1_651_560_386,
            nanos: 60_890_949,
        };
        assert_eq!(
            record_file_name(at),
            "2022-05-03T06_46_26.060890949Z.rcd".to_string()
        );
    }

    #[test]
    fn civil_dates_cover_leap_years_and_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(59), (1970, 3, 1));
        // 2000 is a leap year, 1900 is not; day 11016 is 2000-02-29.
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }

    #[test]
    fn transaction_ids_round_trip_and_reject_the_sdk_form() {
        let id = transaction_id(
            EntityId(1012),
            Timestamp {
                secs: 1_700_000_000,
                nanos: 7,
            },
        );
        assert_eq!(id, "0.0.1012-1700000000-000000007");
        let (payer, start) = parse_transaction_id(&id).expect("round trip");
        assert_eq!(payer, EntityId(1012));
        assert_eq!(start.nanos, 7);
        assert!(parse_transaction_id("0.0.1012@1700000000.000000007").is_err());
    }

    #[test]
    fn timestamp_clauses_carry_their_operator() {
        let at = |secs, nanos| Timestamp { secs, nanos };

        // Seconds alone mean nanosecond zero.
        let eq = parse_timestamp_filter("1700000000").expect("bare seconds");
        assert!(eq.matches(at(1_700_000_000, 0)));
        assert!(!eq.matches(at(1_700_000_000, 1)));

        // A fraction is left-aligned: `.1` is a tenth of a second, not one nanosecond.
        let tenth = parse_timestamp_filter("eq:1700000000.1").expect("fraction");
        assert!(tenth.matches(at(1_700_000_000, 100_000_000)));

        let gte = parse_timestamp_filter("gte:1700000000.000000005").expect("gte");
        assert!(gte.matches(at(1_700_000_000, 5)));
        assert!(gte.matches(at(1_700_000_001, 0)));
        assert!(!gte.matches(at(1_700_000_000, 4)));

        let lt = parse_timestamp_filter("lt:1700000000").expect("lt");
        assert!(lt.matches(at(1_699_999_999, 999_999_999)));
        assert!(!lt.matches(at(1_700_000_000, 0)));

        let ne = parse_timestamp_filter("ne:1700000000").expect("ne");
        assert!(!ne.matches(at(1_700_000_000, 0)));
        assert!(ne.matches(at(1_700_000_001, 0)));
    }

    #[test]
    fn a_timestamp_the_spec_rejects_is_a_400() {
        for bad in [
            "since:1700000000",      // not one of the six operators
            "gte:",                  // no value
            "17000000000000",        // more than 10 digits of seconds
            "1700000000.1234567890", // more than 9 digits of nanos
            "1700000000.",           // trailing dot
            "abc",
        ] {
            assert!(
                parse_timestamp_filter(bad).is_err(),
                "`{bad}` should be refused"
            );
        }
    }

    #[test]
    fn references_accept_ids_and_evm_addresses() {
        assert!(matches!(
            parse_reference("0.0.1002", "accountId"),
            Ok(Reference::Entity(EntityId(1002)))
        ));
        assert!(matches!(
            parse_reference("0x67d8d32e9bf1a9968a5ff53b87d777aa8ebbee69", "accountId"),
            Ok(Reference::Evm(_))
        ));
        // A base32 key alias: a form Hanvil never mints, refused rather than resolved.
        assert!(parse_reference("HIQQEXWKW53RKN4W6XXC4Q232SYNZ3SZ", "accountId").is_err());
    }
}
