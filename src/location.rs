use crate::error::ApiError;
use serde::{Deserialize, Serialize};

pub(crate) const COUNTRY_CODES: &str = "AD AE AF AG AI AL AM AO AQ AR AS AT AU AW AX AZ BA BB BD BE BF BG BH BI BJ BL BM BN BO BQ BR BS BT BV BW BY BZ CA CC CD CF CG CH CI CK CL CM CN CO CR CU CV CW CX CY CZ DE DJ DK DM DO DZ EC EE EG EH ER ES ET FI FJ FK FM FO FR GA GB GD GE GF GG GH GI GL GM GN GP GQ GR GS GT GU GW GY HK HM HN HR HT HU ID IE IL IM IN IO IQ IR IS IT JE JM JO JP KE KG KH KI KM KN KP KR KW KY KZ LA LB LC LI LK LR LS LT LU LV LY MA MC MD ME MF MG MH MK ML MM MN MO MP MQ MR MS MT MU MV MW MX MY MZ NA NC NE NF NG NI NL NO NP NR NU NZ OM PA PE PF PG PH PK PL PM PN PR PS PT PW PY QA RE RO RS RU RW SA SB SC SD SE SG SH SI SJ SK SL SM SN SO SR SS ST SV SX SY SZ TC TD TF TG TH TJ TK TL TM TN TO TR TT TV TW TZ UA UG UM US UY UZ VA VC VE VG VI VN VU WF WS XK YE YT ZA ZM ZW";

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Region {
    #[default]
    Unknown,
    Custom,
    WorldMix,
    Europe,
    Asia,
    NorthernAmerica,
    LatinAmericaAndCaribbean,
    Africa,
    Oceania,
}

impl Region {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Custom => "custom",
            Self::WorldMix => "world_mix",
            Self::Europe => "europe",
            Self::Asia => "asia",
            Self::NorthernAmerica => "northern_america",
            Self::LatinAmericaAndCaribbean => "latin_america_and_caribbean",
            Self::Africa => "africa",
            Self::Oceania => "oceania",
        }
    }
}

pub(crate) fn unknown_country() -> String {
    "unknown".into()
}

pub(crate) fn normalize_country(value: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("unknown") {
        return Ok(unknown_country());
    }
    let value = value.to_ascii_uppercase();
    if !COUNTRY_CODES.split_whitespace().any(|code| code == value) {
        return Err(ApiError::bad_request(
            "invalid_country",
            "Use a supported two-letter country code or unknown",
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_country_codes_or_unknown_only() {
        assert_eq!(normalize_country("us").ok().unwrap(), "US");
        assert_eq!(normalize_country("Unknown").ok().unwrap(), "unknown");
        assert!(normalize_country("ZZ").is_err());
        assert!(normalize_country("Africa").is_err());
    }
}
