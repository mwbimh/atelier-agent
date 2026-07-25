use windows_sys::Win32::NetworkManagement::WindowsFilteringPlatform::FWPM_LAYER_ALE_AUTH_CONNECT_V4;
use windows_sys::Win32::NetworkManagement::WindowsFilteringPlatform::FWPM_LAYER_ALE_AUTH_CONNECT_V6;
use windows_sys::Win32::Networking::WinSock::IPPROTO_TCP;
use windows_sys::Win32::Networking::WinSock::IPPROTO_UDP;
use windows_sys::core::GUID;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ConditionSpec {
    User,
    Protocol(u8),
}

#[derive(Clone, Copy)]
pub(super) struct FilterSpec {
    pub(super) key: GUID,
    pub(super) name: &'static str,
    pub(super) description: &'static str,
    pub(super) layer_key: GUID,
    pub(super) conditions: &'static [ConditionSpec],
}

pub(super) const FILTER_SPECS: &[FilterSpec] = &[
    FilterSpec {
        key: GUID::from_u128(0x8c872fc2_b92f_4685_8d77_a9fb25437eb1),
        name: "atelier_wfp_block_tcp_v4",
        description: "Block Atelier no-network sandbox account TCP outbound IPv4",
        layer_key: FWPM_LAYER_ALE_AUTH_CONNECT_V4,
        conditions: &[
            ConditionSpec::User,
            ConditionSpec::Protocol(IPPROTO_TCP as u8),
        ],
    },
    FilterSpec {
        key: GUID::from_u128(0x1e237599_e5d8_49ec_b1c4_3a5bc43d874e),
        name: "atelier_wfp_block_tcp_v6",
        description: "Block Atelier no-network sandbox account TCP outbound IPv6",
        layer_key: FWPM_LAYER_ALE_AUTH_CONNECT_V6,
        conditions: &[
            ConditionSpec::User,
            ConditionSpec::Protocol(IPPROTO_TCP as u8),
        ],
    },
    FilterSpec {
        key: GUID::from_u128(0x641e10d8_41f5_4c8c_a86d_ad22c0973d53),
        name: "atelier_wfp_block_udp_v4",
        description: "Block Atelier no-network sandbox account UDP outbound IPv4",
        layer_key: FWPM_LAYER_ALE_AUTH_CONNECT_V4,
        conditions: &[
            ConditionSpec::User,
            ConditionSpec::Protocol(IPPROTO_UDP as u8),
        ],
    },
    FilterSpec {
        key: GUID::from_u128(0xb9c807b2_f381_423c_9bd6_1d4eaa644372),
        name: "atelier_wfp_block_udp_v6",
        description: "Block Atelier no-network sandbox account UDP outbound IPv6",
        layer_key: FWPM_LAYER_ALE_AUTH_CONNECT_V6,
        conditions: &[
            ConditionSpec::User,
            ConditionSpec::Protocol(IPPROTO_UDP as u8),
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn guid_key(guid: GUID) -> (u32, u16, u16, [u8; 8]) {
        (guid.data1, guid.data2, guid.data3, guid.data4)
    }

    #[test]
    fn disabled_network_filters_cover_tcp_udp_and_ipv4_ipv6() {
        let shapes = FILTER_SPECS
            .iter()
            .map(|spec| {
                let protocol = spec
                    .conditions
                    .iter()
                    .find_map(|condition| match condition {
                        ConditionSpec::Protocol(protocol) => Some(*protocol),
                        ConditionSpec::User => None,
                    });
                assert!(spec.conditions.contains(&ConditionSpec::User));
                (
                    guid_key(spec.layer_key),
                    protocol.expect("protocol condition"),
                )
            })
            .collect::<BTreeSet<_>>();

        assert_eq!(shapes.len(), 4);
        assert!(shapes.contains(&(guid_key(FWPM_LAYER_ALE_AUTH_CONNECT_V4), IPPROTO_TCP as u8)));
        assert!(shapes.contains(&(guid_key(FWPM_LAYER_ALE_AUTH_CONNECT_V6), IPPROTO_TCP as u8)));
        assert!(shapes.contains(&(guid_key(FWPM_LAYER_ALE_AUTH_CONNECT_V4), IPPROTO_UDP as u8)));
        assert!(shapes.contains(&(guid_key(FWPM_LAYER_ALE_AUTH_CONNECT_V6), IPPROTO_UDP as u8)));
    }

    #[test]
    fn persistent_filter_keys_and_names_are_unique() {
        let keys = FILTER_SPECS
            .iter()
            .map(|spec| {
                (
                    spec.key.data1,
                    spec.key.data2,
                    spec.key.data3,
                    spec.key.data4,
                )
            })
            .collect::<BTreeSet<_>>();
        let names = FILTER_SPECS
            .iter()
            .map(|spec| spec.name)
            .collect::<BTreeSet<_>>();

        assert_eq!(keys.len(), FILTER_SPECS.len());
        assert_eq!(names.len(), FILTER_SPECS.len());
    }
}
