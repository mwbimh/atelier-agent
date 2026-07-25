use super::flow::{RefreshTokenConfig, refresh_credential, validate_provider_id};
use super::types::{OAuthCredential, OAuthError, OAuthHttpClient};
use crate::{CredentialRef, SecretString};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const METADATA_FILE: &str = "oauth.json";
const SECRET_ACCOUNT: &str = "oauth";

pub trait OAuthSecretStore: Send + Sync {
    fn read(&self, service: &str, account: &str) -> Result<Option<SecretString>, OAuthError>;
    fn write(&self, service: &str, account: &str, secret: SecretString) -> Result<(), OAuthError>;
    fn delete(&self, service: &str, account: &str) -> Result<(), OAuthError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemOAuthSecretStore;

impl OAuthSecretStore for SystemOAuthSecretStore {
    fn read(&self, service: &str, account: &str) -> Result<Option<SecretString>, OAuthError> {
        CredentialRef::SecretStore {
            service: service.into(),
            account: account.into(),
        }
        .resolve()
        .map_err(OAuthError::CredentialStore)
    }

    fn write(&self, service: &str, account: &str, secret: SecretString) -> Result<(), OAuthError> {
        CredentialRef::SecretStore {
            service: service.into(),
            account: account.into(),
        }
        .set_secret(secret)
        .map_err(OAuthError::CredentialStore)
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), OAuthError> {
        CredentialRef::SecretStore {
            service: service.into(),
            account: account.into(),
        }
        .delete_secret()
        .map_err(OAuthError::CredentialStore)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCredentialNamespace {
    provider_id: String,
    directory: PathBuf,
    metadata_path: PathBuf,
    service: String,
    account: String,
}

impl ProviderCredentialNamespace {
    fn new(root: &Path, provider_id: &str) -> Result<Self, OAuthError> {
        validate_provider_id(provider_id)?;
        let directory = root
            .join("credentials")
            .join("oauth")
            .join("providers")
            .join(provider_id);
        Ok(Self {
            provider_id: provider_id.into(),
            metadata_path: directory.join(METADATA_FILE),
            directory,
            service: format!("atelier/provider-oauth/{provider_id}"),
            account: SECRET_ACCOUNT.into(),
        })
    }

    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn metadata_path(&self) -> &Path {
        &self.metadata_path
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    pub fn account(&self) -> &str {
        &self.account
    }
}

pub fn resolve_system_access_token(provider_id: &str) -> Result<Option<SecretString>, OAuthError> {
    validate_provider_id(provider_id)?;
    let service = format!("atelier/provider-oauth/{provider_id}");
    let Some(secret) = SystemOAuthSecretStore.read(&service, SECRET_ACCOUNT)? else {
        return Ok(None);
    };
    OAuthCredential::decode_secret(&secret).map(|credential| Some(credential.access_token))
}

pub struct ProviderOAuthCredentialStore<S = SystemOAuthSecretStore> {
    root: PathBuf,
    secrets: S,
}

impl ProviderOAuthCredentialStore<SystemOAuthSecretStore> {
    pub fn system(root: impl Into<PathBuf>) -> Self {
        Self::new(root, SystemOAuthSecretStore)
    }
}

impl<S> ProviderOAuthCredentialStore<S>
where
    S: OAuthSecretStore,
{
    pub fn new(root: impl Into<PathBuf>, secrets: S) -> Self {
        Self {
            root: root.into(),
            secrets,
        }
    }

    pub fn namespace(&self, provider_id: &str) -> Result<ProviderCredentialNamespace, OAuthError> {
        ProviderCredentialNamespace::new(&self.root, provider_id)
    }

    pub fn save(&self, provider_id: &str, credential: &OAuthCredential) -> Result<(), OAuthError> {
        let namespace = self.namespace(provider_id)?;
        fs::create_dir_all(namespace.directory())?;
        self.secrets.write(
            namespace.service(),
            namespace.account(),
            credential.encode_secret()?,
        )?;
        let metadata = CredentialMetadata {
            schema_version: 1,
            provider_id,
            secret_store: SecretStoreMetadata {
                service: namespace.service(),
                account: namespace.account(),
            },
        };
        let encoded =
            serde_json::to_vec_pretty(&metadata).map_err(OAuthError::CredentialEncoding)?;
        let mut temp = tempfile::NamedTempFile::new_in(namespace.directory())?;
        temp.write_all(&encoded)?;
        temp.as_file().sync_all()?;
        crate::persist_provider_temp_file(temp, namespace.metadata_path()).map_err(|error| {
            match error {
                crate::ProviderError::Io(error) => OAuthError::Io(error),
                error => OAuthError::InvalidResponse(error.to_string()),
            }
        })?;
        Ok(())
    }

    pub fn load(&self, provider_id: &str) -> Result<Option<OAuthCredential>, OAuthError> {
        let namespace = self.namespace(provider_id)?;
        if !namespace.metadata_path().is_file() {
            return Ok(None);
        }
        let metadata: CredentialMetadataOwned =
            serde_json::from_slice(&fs::read(namespace.metadata_path())?)
                .map_err(OAuthError::MetadataDecoding)?;
        if metadata.schema_version != 1
            || metadata.provider_id != provider_id
            || metadata.secret_store.service != namespace.service()
            || metadata.secret_store.account != namespace.account()
        {
            return Err(OAuthError::InvalidResponse(
                "Provider OAuth credential metadata does not match its namespace".into(),
            ));
        }
        let secret = self
            .secrets
            .read(namespace.service(), namespace.account())?
            .ok_or_else(|| OAuthError::CredentialMissing(provider_id.into()))?;
        OAuthCredential::decode_secret(&secret).map(Some)
    }

    pub fn delete(&self, provider_id: &str) -> Result<bool, OAuthError> {
        let namespace = self.namespace(provider_id)?;
        if !namespace.metadata_path().is_file() {
            return Ok(false);
        }
        self.secrets
            .delete(namespace.service(), namespace.account())?;
        fs::remove_file(namespace.metadata_path())?;
        if namespace
            .directory()
            .read_dir()
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false)
        {
            let _ = fs::remove_dir(namespace.directory());
        }
        Ok(true)
    }

    pub fn refresh(
        &self,
        client: &dyn OAuthHttpClient,
        config: &RefreshTokenConfig,
    ) -> Result<OAuthCredential, OAuthError> {
        let current = self
            .load(&config.provider_id)?
            .ok_or_else(|| OAuthError::CredentialMissing(config.provider_id.clone()))?;
        let refreshed = refresh_credential(client, config, &current)?;
        self.save(&config.provider_id, &refreshed)?;
        Ok(refreshed)
    }
}

#[derive(Serialize)]
struct CredentialMetadata<'a> {
    schema_version: u32,
    provider_id: &'a str,
    secret_store: SecretStoreMetadata<'a>,
}

#[derive(Serialize)]
struct SecretStoreMetadata<'a> {
    service: &'a str,
    account: &'a str,
}

#[derive(Deserialize)]
struct CredentialMetadataOwned {
    schema_version: u32,
    provider_id: String,
    secret_store: SecretStoreMetadataOwned,
}

#[derive(Deserialize)]
struct SecretStoreMetadataOwned {
    service: String,
    account: String,
}
