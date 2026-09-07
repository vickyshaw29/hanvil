//! Time as the chain sees it. The binary uses the wall clock; tests inject a fixed one. The
//! cheats (`evm_increaseTime`, `evm_setNextBlockTimestamp`) mutate `Chain`, never the clock.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Seconds and nanoseconds since the Unix epoch: the consensus-timestamp shape.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Timestamp {
    /// Whole seconds.
    pub secs: u64,
    /// Nanoseconds within the second, below 10^9.
    pub nanos: u32,
}

impl Timestamp {
    /// Whole-second timestamp.
    #[cfg(test)]
    pub const fn from_secs(secs: u64) -> Self {
        Self { secs, nanos: 0 }
    }

    /// The next representable instant. Consensus timestamps are one nanosecond apart at the
    /// closest, because Hedera uses them to identify a transaction.
    pub const fn next_nano(self) -> Self {
        match self.nanos {
            999_999_999 => Self {
                secs: self.secs + 1,
                nanos: 0,
            },
            nanos => Self {
                secs: self.secs,
                nanos: nanos + 1,
            },
        }
    }
}

/// Mirror node text form: `"1700000000.000000001"` (`openapi.yml` Timestamp pattern).
impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:09}", self.secs, self.nanos)
    }
}

/// Source of the current time.
pub trait Clock: Send + Sync {
    /// Now.
    fn now(&self) -> Timestamp;
}

/// The operating system clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        let since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Timestamp {
            secs: since_epoch.as_secs(),
            nanos: since_epoch.subsec_nanos(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_nano_carries_into_the_next_second() {
        let end = Timestamp {
            secs: 10,
            nanos: 999_999_999,
        };
        assert_eq!(end.next_nano(), Timestamp { secs: 11, nanos: 0 });
        assert_eq!(
            Timestamp { secs: 10, nanos: 0 }.next_nano(),
            Timestamp { secs: 10, nanos: 1 }
        );
    }

    #[test]
    fn mirror_format_pads_nanos() {
        let ts = Timestamp {
            secs: 1_700_000_000,
            nanos: 1,
        };
        assert_eq!(ts.to_string(), "1700000000.000000001");
    }
}
