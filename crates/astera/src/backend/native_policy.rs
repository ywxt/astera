//! Pure native-backend decisions.
//!
//! Keep connector discovery and KMS calls in `native`; this module deliberately accepts plain
//! fixture data so hotplug policy can be tested on unprivileged CI runners.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModeCandidate {
    pub width: u16,
    pub height: u16,
    pub refresh_millihz: u32,
    pub preferred: bool,
}

/// Selects a connector mode deterministically: preferred first, then pixel count and refresh.
pub fn select_mode(modes: &[ModeCandidate]) -> Option<usize> {
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
pub fn stable_output_key(edid: Option<&[u8]>, fallback: &str) -> String {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectorSnapshot {
    pub connector: String,
    pub edid: Option<Vec<u8>>,
    pub modes: Vec<ModeCandidate>,
    pub non_desktop: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannedOutput {
    pub connector: String,
    pub stable_key: String,
    pub mode: ModeCandidate,
}

/// Injectable native-device boundary. Production code obtains equivalent snapshots from DRM;
/// tests provide fixtures without opening a device or becoming DRM master.
pub trait NativeDeviceIo {
    type Error;

    fn connector_snapshots(&self) -> Result<Vec<ConnectorSnapshot>, Self::Error>;
}

/// Adapter used after a platform backend has copied device-owned connector data into fixtures.
pub struct SnapshotSource(pub Vec<ConnectorSnapshot>);

impl NativeDeviceIo for SnapshotSource {
    type Error = std::convert::Infallible;

    fn connector_snapshots(&self) -> Result<Vec<ConnectorSnapshot>, Self::Error> {
        Ok(self.0.clone())
    }
}

pub fn scan_outputs<I: NativeDeviceIo>(io: &I) -> Result<Vec<PlannedOutput>, I::Error> {
    let mut planned = io
        .connector_snapshots()?
        .into_iter()
        .filter_map(|connector| {
            if connector.non_desktop {
                return None;
            }
            let mode = connector.modes.get(select_mode(&connector.modes)?)?;
            Some(PlannedOutput {
                stable_key: stable_output_key(connector.edid.as_deref(), &connector.connector),
                connector: connector.connector,
                mode: *mode,
            })
        })
        .collect::<Vec<_>>();
    planned.sort_by(|left, right| left.connector.cmp(&right.connector));
    Ok(planned)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockDevice {
        connectors: Vec<ConnectorSnapshot>,
    }

    impl NativeDeviceIo for MockDevice {
        type Error = std::convert::Infallible;

        fn connector_snapshots(&self) -> Result<Vec<ConnectorSnapshot>, Self::Error> {
            Ok(self.connectors.clone())
        }
    }

    fn edid_fixture() -> Vec<u8> {
        let mut edid = vec![0_u8; 128];
        edid[8..10].copy_from_slice(&0x1234_u16.to_be_bytes());
        edid[10..12].copy_from_slice(&0x5678_u16.to_le_bytes());
        edid[12..16].copy_from_slice(&0x90abcdef_u32.to_le_bytes());
        edid
    }

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
        let edid = edid_fixture();
        let first = stable_output_key(Some(&edid), "card0-DP-1");
        assert_eq!(first, stable_output_key(Some(&edid), "another-fallback"));
        assert!(first.starts_with("edid:1234:5678:90abcdef:"));
        assert_eq!(stable_output_key(Some(&edid[..8]), "DP-1"), "DP-1");
    }

    #[test]
    fn mock_device_scans_connected_outputs_without_kms() {
        let preferred = ModeCandidate {
            width: 2560,
            height: 1440,
            refresh_millihz: 144_000,
            preferred: true,
        };
        let device = MockDevice {
            connectors: vec![
                ConnectorSnapshot {
                    connector: "HDMI-A-1".into(),
                    edid: None,
                    modes: Vec::new(),
                    non_desktop: false,
                },
                ConnectorSnapshot {
                    connector: "DP-1".into(),
                    edid: Some(edid_fixture()),
                    modes: vec![
                        ModeCandidate {
                            width: 3840,
                            height: 2160,
                            refresh_millihz: 60_000,
                            preferred: false,
                        },
                        preferred,
                    ],
                    non_desktop: false,
                },
            ],
        };

        let outputs = scan_outputs(&device).unwrap();
        assert_eq!(outputs.len(), 1, "mode-less connectors are ignored");
        assert_eq!(outputs[0].connector, "DP-1");
        assert_eq!(outputs[0].mode, preferred);
        assert!(
            outputs[0]
                .stable_key
                .starts_with("edid:1234:5678:90abcdef:")
        );
    }

    #[test]
    fn mock_device_falls_back_to_connector_identity() {
        let mode = ModeCandidate {
            width: 1920,
            height: 1080,
            refresh_millihz: 60_000,
            preferred: true,
        };
        let outputs = scan_outputs(&MockDevice {
            connectors: vec![ConnectorSnapshot {
                connector: "eDP-1".into(),
                edid: None,
                modes: vec![mode],
                non_desktop: false,
            }],
        })
        .unwrap();
        assert_eq!(outputs[0].stable_key, "eDP-1");
        assert_eq!(outputs[0].mode, mode);
    }

    #[test]
    fn non_desktop_connector_is_reserved_instead_of_planned_as_output() {
        let outputs = scan_outputs(&MockDevice {
            connectors: vec![ConnectorSnapshot {
                connector: "DP-2".into(),
                edid: None,
                modes: vec![ModeCandidate {
                    width: 2160,
                    height: 1200,
                    refresh_millihz: 90_000,
                    preferred: true,
                }],
                non_desktop: true,
            }],
        })
        .unwrap();
        assert!(outputs.is_empty());
    }
}
