//! Persistent Windows Filtering Platform rules for the no-network identity.
//!
//! Derived from OpenAI Codex `codex-rs/windows-sandbox-rs/src/wfp.rs` at
//! commit `71448a29e7343b9613eaea620fcdbd196aed2af0`, licensed Apache-2.0.
//! Atelier uses its own stable provider, sublayer and filter GUID namespace,
//! removes Codex telemetry, blocks all TCP/UDP outbound connects, and treats
//! setup failures as fatal instead of continuing without enforcement.

mod filter_specs;

use crate::winutil::to_wide;
use anyhow::{Result, anyhow};
use filter_specs::{ConditionSpec, FILTER_SPECS, FilterSpec};
use std::ffi::{OsStr, c_void};
use std::mem::zeroed;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{
    FWP_E_ALREADY_EXISTS, FWP_E_FILTER_NOT_FOUND, FWP_E_NOT_FOUND, HANDLE, HLOCAL, LocalFree,
};
use windows_sys::Win32::NetworkManagement::WindowsFilteringPlatform::{
    FWP_ACTION_BLOCK, FWP_ACTRL_MATCH_FILTER, FWP_BYTE_BLOB, FWP_CONDITION_VALUE0,
    FWP_CONDITION_VALUE0_0, FWP_EMPTY, FWP_MATCH_EQUAL, FWP_SECURITY_DESCRIPTOR_TYPE, FWP_UINT8,
    FWP_VALUE0, FWPM_ACTION0, FWPM_ACTION0_0, FWPM_ACTRL_READ, FWPM_CONDITION_ALE_USER_ID,
    FWPM_CONDITION_IP_PROTOCOL, FWPM_DISPLAY_DATA0, FWPM_FILTER_CONDITION0,
    FWPM_FILTER_FLAG_PERSISTENT, FWPM_FILTER0, FWPM_FILTER0_0, FWPM_PROVIDER_FLAG_PERSISTENT,
    FWPM_PROVIDER0, FWPM_SESSION0, FWPM_SUBLAYER_FLAG_PERSISTENT, FWPM_SUBLAYER0, FwpmEngineClose0,
    FwpmEngineOpen0, FwpmFilterAdd0, FwpmFilterDeleteByKey0, FwpmFilterGetByKey0,
    FwpmFilterGetSecurityInfoByKey0, FwpmFilterSetSecurityInfoByKey0, FwpmFreeMemory0,
    FwpmProviderAdd0, FwpmProviderDeleteByKey0, FwpmSubLayerAdd0, FwpmSubLayerDeleteByKey0,
    FwpmTransactionAbort0, FwpmTransactionBegin0, FwpmTransactionCommit0,
};
use windows_sys::Win32::Security::Authorization::{
    BuildExplicitAccessWithNameW, BuildSecurityDescriptorW,
    ConvertSecurityDescriptorToStringSecurityDescriptorW, EXPLICIT_ACCESS_W, GRANT_ACCESS,
    SDDL_REVISION_1, SetEntriesInAclW, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};
use windows_sys::Win32::System::Rpc::RPC_C_AUTHN_DEFAULT;
use windows_sys::Win32::System::Threading::INFINITE;
use windows_sys::core::GUID;

const SESSION_NAME: &str = "Atelier Windows Sandbox WFP";
const PROVIDER_NAME: &str = "Atelier Windows Sandbox WFP";
const PROVIDER_DESCRIPTION: &str = "Persistent WFP provider for Atelier sandbox network rules";
const SUBLAYER_NAME: &str = "Atelier Windows Sandbox WFP";
const SUBLAYER_DESCRIPTION: &str = "Persistent WFP sublayer for Atelier sandbox network rules";

// Stable Atelier-owned identities. Changing these would orphan installed WFP
// objects, so updates must preserve them and use explicit cleanup migrations.
const PROVIDER_KEY: GUID = GUID::from_u128(0x6f1e7d2c_4a3b_4cc2_a6f4_0e8cb7ad8921);
const SUBLAYER_KEY: GUID = GUID::from_u128(0x32d0e87f_9d78_4d69_bf2e_8a2ff7a6c935);

pub(crate) fn install_disabled_network_filters_for_account(account: &str) -> Result<usize> {
    let engine = Engine::open()?;
    let mut transaction = engine.begin_transaction()?;
    ensure_provider(engine.handle)?;
    ensure_sublayer(engine.handle)?;

    let user_condition = UserMatchCondition::for_account(account)?;
    for spec in FILTER_SPECS {
        delete_filter_if_present(engine.handle, &spec.key)?;
        add_filter(engine.handle, spec, &user_condition)?;
    }
    transaction.commit()?;
    for spec in FILTER_SPECS {
        grant_unelevated_filter_read(engine.handle, &spec.key)?;
    }

    let expected_sid = crate::token::LocalSid::from_account(account)?.to_string()?;
    if !disabled_network_filters_installed_for_sid(&expected_sid)? {
        return Err(anyhow!(
            "Atelier WFP filters were added but could not be verified after commit"
        ));
    }
    Ok(FILTER_SPECS.len())
}

pub(crate) fn disabled_network_filters_installed_for_sid(expected_sid: &str) -> Result<bool> {
    let engine = Engine::open()?;
    for spec in FILTER_SPECS {
        if !filter_matches(engine.handle, spec, expected_sid)? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn remove_disabled_network_filters() -> Result<()> {
    let engine = Engine::open()?;
    let mut transaction = engine.begin_transaction()?;
    for spec in FILTER_SPECS {
        delete_filter_if_present(engine.handle, &spec.key)?;
    }
    delete_sublayer_if_present(engine.handle)?;
    delete_provider_if_present(engine.handle)?;
    transaction.commit()
}

struct Engine {
    handle: HANDLE,
}

impl Engine {
    fn open() -> Result<Self> {
        let session_name = to_wide(OsStr::new(SESSION_NAME));
        let mut session: FWPM_SESSION0 = unsafe { zeroed() };
        session.displayData = FWPM_DISPLAY_DATA0 {
            name: session_name.as_ptr().cast_mut(),
            description: null_mut(),
        };
        session.txnWaitTimeoutInMSec = INFINITE;

        let mut handle = HANDLE::default();
        let result = unsafe {
            FwpmEngineOpen0(
                null(),
                RPC_C_AUTHN_DEFAULT as u32,
                null(),
                &session,
                &mut handle,
            )
        };
        ensure_success(result, "FwpmEngineOpen0")?;
        Ok(Self { handle })
    }

    fn begin_transaction(&self) -> Result<Transaction<'_>> {
        let result = unsafe { FwpmTransactionBegin0(self.handle, 0) };
        ensure_success(result, "FwpmTransactionBegin0")?;
        Ok(Transaction {
            engine: self,
            committed: false,
        })
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        unsafe { FwpmEngineClose0(self.handle) };
    }
}

struct Transaction<'a> {
    engine: &'a Engine,
    committed: bool,
}

impl Transaction<'_> {
    fn commit(&mut self) -> Result<()> {
        let result = unsafe { FwpmTransactionCommit0(self.engine.handle) };
        ensure_success(result, "FwpmTransactionCommit0")?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.committed {
            unsafe { FwpmTransactionAbort0(self.engine.handle) };
        }
    }
}

struct UserMatchCondition {
    security_descriptor: PSECURITY_DESCRIPTOR,
    blob: FWP_BYTE_BLOB,
}

impl UserMatchCondition {
    fn for_account(account: &str) -> Result<Self> {
        let account_w = to_wide(OsStr::new(account));
        let mut access: EXPLICIT_ACCESS_W = unsafe { zeroed() };
        unsafe {
            BuildExplicitAccessWithNameW(
                &mut access,
                account_w.as_ptr(),
                FWP_ACTRL_MATCH_FILTER,
                GRANT_ACCESS,
                0,
            );
        }

        let mut security_descriptor: PSECURITY_DESCRIPTOR = null_mut();
        let mut security_descriptor_len = 0;
        let result = unsafe {
            BuildSecurityDescriptorW(
                null(),
                null(),
                1,
                &access,
                0,
                null(),
                null_mut(),
                &mut security_descriptor_len,
                &mut security_descriptor,
            )
        };
        ensure_success(result, "BuildSecurityDescriptorW")?;

        Ok(Self {
            security_descriptor,
            blob: FWP_BYTE_BLOB {
                size: security_descriptor_len,
                data: security_descriptor.cast(),
            },
        })
    }
}

impl Drop for UserMatchCondition {
    fn drop(&mut self) {
        if !self.security_descriptor.is_null() {
            unsafe { LocalFree(self.security_descriptor as HLOCAL) };
        }
    }
}

fn ensure_provider(engine: HANDLE) -> Result<()> {
    let name = to_wide(OsStr::new(PROVIDER_NAME));
    let description = to_wide(OsStr::new(PROVIDER_DESCRIPTION));
    let provider = FWPM_PROVIDER0 {
        providerKey: PROVIDER_KEY,
        displayData: FWPM_DISPLAY_DATA0 {
            name: name.as_ptr().cast_mut(),
            description: description.as_ptr().cast_mut(),
        },
        flags: FWPM_PROVIDER_FLAG_PERSISTENT,
        providerData: empty_blob(),
        serviceName: null_mut(),
    };
    let result = unsafe { FwpmProviderAdd0(engine, &provider, null_mut()) };
    ensure_success_or(result, "FwpmProviderAdd0", &[FWP_E_ALREADY_EXISTS as u32])
}

fn ensure_sublayer(engine: HANDLE) -> Result<()> {
    let name = to_wide(OsStr::new(SUBLAYER_NAME));
    let description = to_wide(OsStr::new(SUBLAYER_DESCRIPTION));
    let provider_key = PROVIDER_KEY;
    let sublayer = FWPM_SUBLAYER0 {
        subLayerKey: SUBLAYER_KEY,
        displayData: FWPM_DISPLAY_DATA0 {
            name: name.as_ptr().cast_mut(),
            description: description.as_ptr().cast_mut(),
        },
        flags: FWPM_SUBLAYER_FLAG_PERSISTENT,
        providerKey: &provider_key as *const _ as *mut _,
        providerData: empty_blob(),
        weight: 0x8000,
    };
    let result = unsafe { FwpmSubLayerAdd0(engine, &sublayer, null_mut()) };
    ensure_success_or(result, "FwpmSubLayerAdd0", &[FWP_E_ALREADY_EXISTS as u32])
}

fn add_filter(engine: HANDLE, spec: &FilterSpec, user: &UserMatchCondition) -> Result<()> {
    let name = to_wide(OsStr::new(spec.name));
    let description = to_wide(OsStr::new(spec.description));
    let mut conditions = build_conditions(spec.conditions, user);
    let provider_key = PROVIDER_KEY;
    let filter = FWPM_FILTER0 {
        filterKey: spec.key,
        displayData: FWPM_DISPLAY_DATA0 {
            name: name.as_ptr().cast_mut(),
            description: description.as_ptr().cast_mut(),
        },
        flags: FWPM_FILTER_FLAG_PERSISTENT,
        providerKey: &provider_key as *const _ as *mut _,
        providerData: empty_blob(),
        layerKey: spec.layer_key,
        subLayerKey: SUBLAYER_KEY,
        weight: empty_value(),
        numFilterConditions: conditions.len() as u32,
        filterCondition: conditions.as_mut_ptr(),
        action: FWPM_ACTION0 {
            r#type: FWP_ACTION_BLOCK,
            Anonymous: FWPM_ACTION0_0 {
                filterType: GUID::from_u128(0),
            },
        },
        Anonymous: FWPM_FILTER0_0 { rawContext: 0 },
        reserved: null_mut(),
        filterId: 0,
        effectiveWeight: empty_value(),
    };
    let mut filter_id = 0;
    let result = unsafe { FwpmFilterAdd0(engine, &filter, null_mut(), &mut filter_id) };
    ensure_success(result, &format!("FwpmFilterAdd0({})", spec.name))
}

fn grant_unelevated_filter_read(engine: HANDLE, key: &GUID) -> Result<()> {
    let local_users = crate::token::LocalSid::new("S-1-5-32-545")?;
    let mut dacl = null_mut();
    let mut security_descriptor = null_mut();
    let result = unsafe {
        FwpmFilterGetSecurityInfoByKey0(
            engine,
            key,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut security_descriptor,
        )
    };
    ensure_success(result, "FwpmFilterGetSecurityInfoByKey0")?;

    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FWPM_ACTRL_READ,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: 0,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: local_users.as_ptr().cast(),
        },
    };
    let mut readable_dacl = null_mut();
    let acl_result = unsafe { SetEntriesInAclW(1, &entry, dacl, &mut readable_dacl) };
    if !security_descriptor.is_null() {
        let mut memory = security_descriptor.cast::<c_void>();
        unsafe { FwpmFreeMemory0(&mut memory) };
    }
    ensure_success(acl_result, "SetEntriesInAclW(filter status read)")?;

    let set_result = unsafe {
        FwpmFilterSetSecurityInfoByKey0(
            engine,
            key,
            DACL_SECURITY_INFORMATION,
            null(),
            null(),
            readable_dacl,
            null(),
        )
    };
    unsafe { LocalFree(readable_dacl as HLOCAL) };
    ensure_success(set_result, "FwpmFilterSetSecurityInfoByKey0")
}

fn build_conditions(
    specs: &[ConditionSpec],
    user: &UserMatchCondition,
) -> Vec<FWPM_FILTER_CONDITION0> {
    specs
        .iter()
        .map(|spec| match spec {
            ConditionSpec::User => FWPM_FILTER_CONDITION0 {
                fieldKey: FWPM_CONDITION_ALE_USER_ID,
                matchType: FWP_MATCH_EQUAL,
                conditionValue: FWP_CONDITION_VALUE0 {
                    r#type: FWP_SECURITY_DESCRIPTOR_TYPE,
                    Anonymous: FWP_CONDITION_VALUE0_0 {
                        sd: &user.blob as *const _ as *mut _,
                    },
                },
            },
            ConditionSpec::Protocol(protocol) => FWPM_FILTER_CONDITION0 {
                fieldKey: FWPM_CONDITION_IP_PROTOCOL,
                matchType: FWP_MATCH_EQUAL,
                conditionValue: FWP_CONDITION_VALUE0 {
                    r#type: FWP_UINT8,
                    Anonymous: FWP_CONDITION_VALUE0_0 { uint8: *protocol },
                },
            },
        })
        .collect()
}

struct OwnedFilter(*mut FWPM_FILTER0);

impl Drop for OwnedFilter {
    fn drop(&mut self) {
        if !self.0.is_null() {
            let mut memory = self.0.cast::<c_void>();
            unsafe { FwpmFreeMemory0(&mut memory) };
        }
    }
}

fn filter_matches(engine: HANDLE, spec: &FilterSpec, expected_sid: &str) -> Result<bool> {
    let mut filter: *mut FWPM_FILTER0 = null_mut();
    let result = unsafe { FwpmFilterGetByKey0(engine, &spec.key, &mut filter) };
    if result == FWP_E_FILTER_NOT_FOUND as u32 || result == FWP_E_NOT_FOUND as u32 {
        return Ok(false);
    }
    ensure_success(result, "FwpmFilterGetByKey0")?;
    let filter = OwnedFilter(filter);
    if filter.0.is_null() {
        return Ok(false);
    }
    let filter = unsafe { &*filter.0 };
    if !guid_equal(filter.filterKey, spec.key)
        || !guid_equal(filter.layerKey, spec.layer_key)
        || !guid_equal(filter.subLayerKey, SUBLAYER_KEY)
        || filter.action.r#type != FWP_ACTION_BLOCK
        || filter.flags & FWPM_FILTER_FLAG_PERSISTENT == 0
        || filter.providerKey.is_null()
        || !guid_equal(unsafe { *filter.providerKey }, PROVIDER_KEY)
    {
        return Ok(false);
    }
    let conditions = if filter.numFilterConditions == 0 || filter.filterCondition.is_null() {
        &[]
    } else {
        unsafe {
            std::slice::from_raw_parts(filter.filterCondition, filter.numFilterConditions as usize)
        }
    };
    let has_expected_user = conditions.iter().any(|condition| {
        guid_equal(condition.fieldKey, FWPM_CONDITION_ALE_USER_ID)
            && condition.matchType == FWP_MATCH_EQUAL
            && condition.conditionValue.r#type == FWP_SECURITY_DESCRIPTOR_TYPE
            && security_descriptor_contains_sid(
                unsafe { condition.conditionValue.Anonymous.sd },
                expected_sid,
            )
            .unwrap_or(false)
    });
    let expected_protocol = spec
        .conditions
        .iter()
        .find_map(|condition| match condition {
            ConditionSpec::Protocol(protocol) => Some(*protocol),
            ConditionSpec::User => None,
        });
    let has_expected_protocol = expected_protocol.is_some_and(|expected| {
        conditions.iter().any(|condition| {
            guid_equal(condition.fieldKey, FWPM_CONDITION_IP_PROTOCOL)
                && condition.matchType == FWP_MATCH_EQUAL
                && condition.conditionValue.r#type == FWP_UINT8
                && unsafe { condition.conditionValue.Anonymous.uint8 } == expected
        })
    });
    Ok(has_expected_user && has_expected_protocol)
}

fn security_descriptor_contains_sid(blob: *mut FWP_BYTE_BLOB, expected_sid: &str) -> Result<bool> {
    if blob.is_null() {
        return Ok(false);
    }
    let blob = unsafe { &*blob };
    if blob.data.is_null() || blob.size == 0 {
        return Ok(false);
    }
    let mut sddl = null_mut();
    let result = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            blob.data.cast(),
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut sddl,
            null_mut(),
        )
    };
    if result == 0 {
        return Err(crate::winutil::win_error(
            "ConvertSecurityDescriptorToStringSecurityDescriptorW",
        ));
    }
    let mut len = 0;
    while unsafe { *sddl.add(len) } != 0 {
        len += 1;
    }
    let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(sddl, len) });
    unsafe { LocalFree(sddl as HLOCAL) };
    Ok(text.contains(expected_sid))
}

fn delete_filter_if_present(engine: HANDLE, key: &GUID) -> Result<()> {
    let result = unsafe { FwpmFilterDeleteByKey0(engine, key) };
    ensure_success_or(
        result,
        "FwpmFilterDeleteByKey0",
        &[FWP_E_FILTER_NOT_FOUND as u32, FWP_E_NOT_FOUND as u32],
    )
}

fn guid_equal(left: GUID, right: GUID) -> bool {
    left.data1 == right.data1
        && left.data2 == right.data2
        && left.data3 == right.data3
        && left.data4 == right.data4
}

fn delete_sublayer_if_present(engine: HANDLE) -> Result<()> {
    let result = unsafe { FwpmSubLayerDeleteByKey0(engine, &SUBLAYER_KEY) };
    ensure_success_or(
        result,
        "FwpmSubLayerDeleteByKey0",
        &[FWP_E_NOT_FOUND as u32],
    )
}

fn delete_provider_if_present(engine: HANDLE) -> Result<()> {
    let result = unsafe { FwpmProviderDeleteByKey0(engine, &PROVIDER_KEY) };
    ensure_success_or(
        result,
        "FwpmProviderDeleteByKey0",
        &[FWP_E_NOT_FOUND as u32],
    )
}

fn ensure_success(result: u32, operation: &str) -> Result<()> {
    ensure_success_or(result, operation, &[])
}

fn ensure_success_or(result: u32, operation: &str, allowed: &[u32]) -> Result<()> {
    if result == 0 || allowed.contains(&result) {
        Ok(())
    } else {
        Err(anyhow!("{operation} failed: 0x{result:08X}"))
    }
}

fn empty_blob() -> FWP_BYTE_BLOB {
    FWP_BYTE_BLOB {
        size: 0,
        data: null_mut(),
    }
}

fn empty_value() -> FWP_VALUE0 {
    FWP_VALUE0 {
        r#type: FWP_EMPTY,
        Anonymous: unsafe { zeroed() },
    }
}
