//! Pure native-backend decisions.
//!
//! Keep connector discovery and KMS calls in `native`; this module deliberately accepts plain
//! fixture data so hotplug policy can be tested on unprivileged CI runners.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ModeCandidate {
    pub width: u16,
    pub height: u16,
    pub refresh_millihz: u32,
    pub preferred: bool,
}

/// Selects a connector mode deterministically: preferred first, then pixel count and refresh.
pub(crate) fn select_mode(modes: &[ModeCandidate]) -> Option<usize> {
    modes
        .iter()
        .enumerate()
        .max_by_key(|(_, mode)| {
            (
                mode.preferred,
                u64::from(mode.width) * u64::from(mode.height),
                mode.refresh_millihz,
            )
        })
        .map(|(index, _)| index)
}

/// Builds a stable key without requiring a DRM property blob handle.
pub(crate) fn stable_output_key(edid: Option<&[u8]>, fallback: &str) -> String {
    let Some(edid) = edid.filter(|bytes| bytes.len() >= 16) else {
        return fallback.to_owned();
    };
    let manufacturer = u16::from_be_bytes([edid[8], edid[9]]);
    let product = u16::from_le_bytes([edid[10], edid[11]]);
    let serial = u32::from_le_bytes([edid[12], edid[13], edid[14], edid[15]]);
    let fingerprint = edid.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    });
    format!("edid:{manufacturer:04x}:{product:04x}:{serial:08x}:{fingerprint:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferred_mode_wins_over_larger_fallback() {
        let modes = [
            ModeCandidate {
                width: 3840,
                height: 2160,
                refresh_millihz: 60_000,
                preferred: false,
            },
            ModeCandidate {
                width: 2560,
                height: 1440,
                refresh_millihz: 144_000,
                preferred: true,
            },
        ];
        assert_eq!(select_mode(&modes), Some(1));
    }

    #[test]
    fn best_fallback_is_deterministic() {
        let modes = [
            ModeCandidate {
                width: 1920,
                height: 1080,
                refresh_millihz: 60_000,
                preferred: false,
            },
            ModeCandidate {
                width: 1920,
                height: 1080,
                refresh_millihz: 120_000,
                preferred: false,
            },
        ];
        assert_eq!(select_mode(&modes), Some(1));
        assert_eq!(select_mode(&[]), None);
    }

    #[test]
    fn edid_fixture_produces_stable_identity() {
        let mut edid = [0_u8; 128];
        edid[8..10].copy_from_slice(&0x1234_u16.to_be_bytes());
        edid[10..12].copy_from_slice(&0x5678_u16.to_le_bytes());
        edid[12..16].copy_from_slice(&0x90abcdef_u32.to_le_bytes());
        let first = stable_output_key(Some(&edid), "card0-DP-1");
        assert_eq!(first, stable_output_key(Some(&edid), "another-fallback"));
        assert!(first.starts_with("edid:1234:5678:90abcdef:"));
        assert_eq!(stable_output_key(Some(&edid[..8]), "DP-1"), "DP-1");
    }
}
