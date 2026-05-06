// SPDX-License-Identifier: FSL-1.1-MIT
//! The four AppRafter deployment tiers (see spec.md §0 and §4.1).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::CliError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Solo,
    Team,
    Prod,
    Regulated,
}

impl Tier {
    pub const fn level(self) -> u8 {
        match self {
            Tier::Solo => 1,
            Tier::Team => 2,
            Tier::Prod => 3,
            Tier::Regulated => 4,
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Tier::Solo => "solo",
            Tier::Team => "team",
            Tier::Prod => "prod",
            Tier::Regulated => "regulated",
        };
        f.write_str(s)
    }
}

impl FromStr for Tier {
    type Err = CliError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "solo" => Ok(Tier::Solo),
            "team" => Ok(Tier::Team),
            "prod" => Ok(Tier::Prod),
            "regulated" => Ok(Tier::Regulated),
            other => Err(CliError::Other(format!(
                "unknown tier `{other}`; expected one of: solo, team, prod, regulated"
            ))),
        }
    }
}
