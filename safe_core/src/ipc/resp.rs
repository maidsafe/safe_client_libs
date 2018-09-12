// Copyright 2018 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

#![allow(unsafe_code)]

use client::MDataInfo;
use ffi::ipc::resp as ffi;
use ffi_utils::{vec_into_raw_parts, ReprC, StringError};
use ipc::req::{
    container_perms_from_repr_c, container_perms_into_repr_c, permission_set_clone_from_repr_c,
    permission_set_into_repr_c, ContainerPermissions,
};
use ipc::IpcError;
use maidsafe_utilities::serialisation::{deserialise, serialise};
use routing::{BootstrapConfig, PermissionSet, Value, XorName};
use safe_crypto::{
    self, Nonce, PublicEncryptKey, PublicSignKey, SecretEncryptKey, SecretSignKey, SymmetricKey,
    NONCE_BYTES,
};
use std::collections::HashMap;
use std::ffi::{CString, NulError};
use std::{ptr, slice};

/// Entry key under which the metadata are stored.
#[no_mangle]
pub static METADATA_KEY: &'static [u8] = b"_metadata";
/// Length of the metadata key.
// IMPORTANT: make sure this value stays in sync with the actual length of `METADATA_KEY`!
#[no_mangle]
pub static METADATA_KEY_LEN: usize = 9;

/// IPC response.
// TODO: `TransOwnership` variant
#[cfg_attr(feature = "cargo-clippy", allow(large_enum_variant))]
#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum IpcResp {
    /// Authentication.
    Auth(Result<AuthGranted, IpcError>),
    /// Containers.
    Containers(Result<(), IpcError>),
    /// Unregistered client.
    Unregistered(Result<BootstrapConfig, IpcError>),
    /// Share mutable data.
    ShareMData(Result<(), IpcError>),
}

/// It represents the authentication response.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct AuthGranted {
    /// The access keys.
    pub app_keys: AppKeys,

    /// The crust config.
    /// Useful to reuse bootstrap nodes and speed up access.
    pub bootstrap_config: BootstrapConfig,

    /// Access container info
    pub access_container_info: AccessContInfo,
    /// Access container entry
    pub access_container_entry: AccessContainerEntry,
}

impl AuthGranted {
    /// Consumes the object and returns the wrapped raw pointer
    ///
    /// You're now responsible for freeing this memory once you're done.
    pub fn into_repr_c(self) -> Result<ffi::AuthGranted, IpcError> {
        let AuthGranted {
            app_keys,
            bootstrap_config,
            access_container_info,
            access_container_entry,
        } = self;
        let bootstrap_config = serialise(&bootstrap_config)?;
        let (ptr, len, cap) = vec_into_raw_parts(bootstrap_config);

        Ok(ffi::AuthGranted {
            app_keys: app_keys.into_repr_c(),
            access_container_info: access_container_info.into_repr_c(),
            access_container_entry: access_container_entry_into_repr_c(access_container_entry)?,
            bootstrap_config: ptr,
            bootstrap_config_len: len,
            bootstrap_config_cap: cap,
        })
    }
}

impl ReprC for AuthGranted {
    type C = *const ffi::AuthGranted;
    type Error = IpcError;

    unsafe fn clone_from_repr_c(repr_c: Self::C) -> Result<Self, Self::Error> {
        let ffi::AuthGranted {
            app_keys,
            bootstrap_config,
            bootstrap_config_len,
            access_container_info,
            ref access_container_entry,
            ..
        } = *repr_c;
        let bootstrap_config = slice::from_raw_parts(bootstrap_config, bootstrap_config_len);
        let bootstrap_config = deserialise(bootstrap_config)?;

        Ok(AuthGranted {
            app_keys: AppKeys::clone_from_repr_c(app_keys)?,
            bootstrap_config,
            access_container_info: AccessContInfo::clone_from_repr_c(access_container_info)?,
            access_container_entry: access_container_entry_clone_from_repr_c(
                access_container_entry,
            )?,
        })
    }
}

/// Represents the needed keys to work with the data.
#[derive(Clone, Serialize, Deserialize, Debug, Eq, PartialEq)]
pub struct AppKeys {
    /// Owner signing public key.
    pub owner_key: PublicSignKey,
    /// Data symmetric encryption key.
    pub enc_key: SymmetricKey,
    /// Asymmetric sign public key.
    ///
    /// This is the identity of the App in the Network.
    pub sign_pk: PublicSignKey,
    /// Asymmetric sign private key.
    pub sign_sk: SecretSignKey,
    /// Asymmetric enc public key.
    pub enc_pk: PublicEncryptKey,
    /// Asymmetric enc private key.
    pub enc_sk: SecretEncryptKey,
}

impl AppKeys {
    /// Generate random keys
    pub fn random(owner_key: PublicSignKey) -> AppKeys {
        let (enc_pk, enc_sk) = safe_crypto::gen_encrypt_keypair();
        let (sign_pk, sign_sk) = safe_crypto::gen_sign_keypair();
        let enc_key = SymmetricKey::new();

        AppKeys {
            owner_key,
            enc_key,
            sign_pk,
            sign_sk,
            enc_pk,
            enc_sk,
        }
    }

    /// Consumes the object and returns the wrapped raw pointer
    ///
    /// You're now responsible for freeing this memory once you're done.
    pub fn into_repr_c(self) -> ffi::AppKeys {
        let AppKeys {
            owner_key,
            enc_key,
            sign_pk,
            sign_sk,
            enc_pk,
            enc_sk,
        } = self;
        ffi::AppKeys {
            owner_key: owner_key.into_bytes(),
            enc_key: enc_key.into_bytes(),
            sign_pk: sign_pk.into_bytes(),
            sign_sk: sign_sk.into_bytes(),
            enc_pk: enc_pk.into_bytes(),
            enc_sk: enc_sk.into_bytes(),
        }
    }
}

impl ReprC for AppKeys {
    type C = ffi::AppKeys;
    type Error = IpcError;

    unsafe fn clone_from_repr_c(raw: Self::C) -> Result<Self, Self::Error> {
        Ok(AppKeys {
            owner_key: PublicSignKey::from_bytes(raw.owner_key),
            enc_key: SymmetricKey::from_bytes(raw.enc_key),
            sign_pk: PublicSignKey::from_bytes(raw.sign_pk),
            sign_sk: SecretSignKey::from_bytes(raw.sign_sk),
            enc_pk: PublicEncryptKey::from_bytes(raw.enc_pk),
            enc_sk: SecretEncryptKey::from_bytes(raw.enc_sk),
        })
    }
}

/// Represents an entry for a single app in the access container
pub type AccessContainerEntry = HashMap<String, (MDataInfo, ContainerPermissions)>;

/// Convert `AccessContainerEntry` to FFI representation.
pub fn access_container_entry_into_repr_c(
    entry: AccessContainerEntry,
) -> Result<ffi::AccessContainerEntry, NulError> {
    let mut vec = Vec::with_capacity(entry.len());

    for (name, (mdata_info, permissions)) in entry {
        vec.push(ffi::ContainerInfo {
            name: CString::new(name)?.into_raw(),
            mdata_info: mdata_info.into_repr_c(),
            permissions: container_perms_into_repr_c(&permissions),
        })
    }

    let (containers, containers_len, containers_cap) = vec_into_raw_parts(vec);
    Ok(ffi::AccessContainerEntry {
        containers,
        containers_len,
        containers_cap,
    })
}

/// Convert FFI representation of `AccessContainerEntry` to native rust representation by cloning.
pub unsafe fn access_container_entry_clone_from_repr_c(
    entry: *const ffi::AccessContainerEntry,
) -> Result<AccessContainerEntry, IpcError> {
    let input = slice::from_raw_parts((*entry).containers, (*entry).containers_len);
    let mut output = AccessContainerEntry::with_capacity(input.len());

    for container in input {
        let name = String::clone_from_repr_c(container.name)?;
        let mdata_info = MDataInfo::clone_from_repr_c(&container.mdata_info)?;
        let permissions = container_perms_from_repr_c(container.permissions)?;

        let _ = output.insert(name, (mdata_info, permissions));
    }

    Ok(output)
}

/// Access container
#[derive(Clone, Serialize, Deserialize, Debug, Eq, PartialEq)]
pub struct AccessContInfo {
    /// ID
    pub id: XorName,
    /// Type tag
    pub tag: u64,
    /// Nonce
    pub nonce: Nonce,
}

impl AccessContInfo {
    /// Consumes the object and returns the wrapped raw pointer
    ///
    /// You're now responsible for freeing this memory once you're done.
    pub fn into_repr_c(self) -> ffi::AccessContInfo {
        let AccessContInfo { id, tag, nonce } = self;
        let mut nonce_bytes: [u8; NONCE_BYTES] = Default::default();
        nonce_bytes.copy_from_slice(&nonce.into_bytes());

        ffi::AccessContInfo {
            id: id.0,
            tag,
            nonce: nonce_bytes,
        }
    }

    /// Creates `MDataInfo` from this `AccessContInfo`
    pub fn into_mdata_info(self, enc_key: SymmetricKey) -> MDataInfo {
        MDataInfo::new_private(self.id, self.tag, (enc_key, self.nonce))
    }

    /// Creates an `AccessContInfo` from a given `MDataInfo`
    pub fn from_mdata_info(md: &MDataInfo) -> Result<AccessContInfo, IpcError> {
        if let Some((_, ref nonce)) = md.enc_info {
            Ok(AccessContInfo {
                id: md.name,
                tag: md.type_tag,
                nonce: nonce.clone(),
            })
        } else {
            Err(IpcError::Unexpected(
                "MDataInfo doesn't contain nonce".to_owned(),
            ))
        }
    }
}

impl ReprC for AccessContInfo {
    type C = ffi::AccessContInfo;
    type Error = IpcError;

    unsafe fn clone_from_repr_c(repr_c: Self::C) -> Result<Self, Self::Error> {
        Ok(AccessContInfo {
            id: XorName(repr_c.id),
            tag: repr_c.tag,
            nonce: Nonce::from_bytes(repr_c.nonce),
        })
    }
}

/// Encrypts and serialises an access container key using given app ID and app key
pub fn access_container_enc_key(
    app_id: &str,
    app_enc_key: &SymmetricKey,
    access_container_nonce: &Nonce,
) -> Result<Vec<u8>, IpcError> {
    let key = app_id.as_bytes();
    let mut key_pt = key.to_vec();
    let ac_nonce_bytes = access_container_nonce.clone().into_bytes();
    key_pt.extend_from_slice(&ac_nonce_bytes);

    let mut nonce_bytes: [u8; NONCE_BYTES] = Default::default();
    nonce_bytes.copy_from_slice(&safe_crypto::hash(&key_pt)[..NONCE_BYTES]);
    let key_nonce = Nonce::from_bytes(nonce_bytes);

    Ok(app_enc_key.encrypt_bytes_with_nonce(key, &key_nonce)?)
}

/// Information about an app that has access to an MD through `sign_key`
#[derive(Debug)]
pub struct AppAccess {
    /// App's or user's public key
    pub sign_key: PublicSignKey,
    /// A list of permissions
    pub permissions: PermissionSet,
    /// App's user-facing name
    pub name: Option<String>,
    /// App id
    pub app_id: Option<String>,
}

impl AppAccess {
    /// Consumes the object and returns the wrapped raw pointer.
    ///
    /// You're now responsible for freeing this memory once you're done.
    pub fn into_repr_c(self) -> Result<ffi::AppAccess, IpcError> {
        let AppAccess {
            sign_key,
            permissions,
            name,
            app_id,
        } = self;

        let name = match name {
            Some(name) => CString::new(name).map_err(StringError::from)?.into_raw(),
            None => ptr::null(),
        };

        let app_id = match app_id {
            Some(app_id) => CString::new(app_id).map_err(StringError::from)?.into_raw(),
            None => ptr::null(),
        };

        Ok(ffi::AppAccess {
            sign_key: sign_key.into_bytes(),
            permissions: permission_set_into_repr_c(permissions),
            name,
            app_id,
        })
    }
}

impl ReprC for AppAccess {
    type C = *const ffi::AppAccess;
    type Error = IpcError;

    unsafe fn clone_from_repr_c(repr_c: Self::C) -> Result<Self, Self::Error> {
        Ok(AppAccess {
            sign_key: PublicSignKey::from_bytes((*repr_c).sign_key),
            permissions: permission_set_clone_from_repr_c((*repr_c).permissions)?,
            name: Some(String::clone_from_repr_c((*repr_c).name)?),
            app_id: Some(String::clone_from_repr_c((*repr_c).app_id)?),
        })
    }
}

/// Metadata for `MutableData`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UserMetadata {
    /// Name or purpose of this mutable data.
    pub name: Option<String>,
    /// Description of how this mutable data should or should not be shared.
    pub description: Option<String>,
}

impl UserMetadata {
    /// Converts this object into an FFI representation with more information.
    pub fn into_md_response(
        self,
        xor_name: XorName,
        type_tag: u64,
    ) -> Result<ffi::MetadataResponse, NulError> {
        Ok(ffi::MetadataResponse {
            name: match self.name {
                Some(name) => CString::new(name)?.into_raw(),
                None => ptr::null(),
            },
            description: match self.description {
                Some(description) => CString::new(description)?.into_raw(),
                None => ptr::null(),
            },
            xor_name: xor_name.0,
            type_tag,
        })
    }
}

impl ReprC for UserMetadata {
    type C = *const ffi::MetadataResponse;
    type Error = IpcError;

    unsafe fn clone_from_repr_c(repr_c: Self::C) -> Result<Self, Self::Error> {
        Ok(UserMetadata {
            name: if (*repr_c).name.is_null() {
                None
            } else {
                Some(String::clone_from_repr_c((*repr_c).name)?)
            },
            description: if (*repr_c).description.is_null() {
                None
            } else {
                Some(String::clone_from_repr_c((*repr_c).description)?)
            },
        })
    }
}

/// Redefine the Value from routing so that we can `impl ReprC`.
#[derive(Hash, Eq, PartialEq, PartialOrd, Ord, Clone, Serialize, Deserialize, Debug)]
pub struct MDataValue {
    /// Content of the entry.
    pub content: Vec<u8>,
    /// Version of the entry.
    pub entry_version: u64,
}

impl MDataValue {
    /// Convert routing representation to `MDataValue`.
    pub fn from_routing(value: Value) -> Self {
        MDataValue {
            content: value.content,
            entry_version: value.entry_version,
        }
    }

    /// Returns FFI counterpart without consuming the object.
    pub fn as_repr_c(&self) -> ffi::MDataValue {
        ffi::MDataValue {
            content: self.content.as_ptr(),
            content_len: self.content.len(),
            entry_version: self.entry_version,
        }
    }
}

impl ReprC for MDataValue {
    type C = *const ffi::MDataValue;
    type Error = ();

    unsafe fn clone_from_repr_c(c_repr: Self::C) -> Result<Self, Self::Error> {
        let ffi::MDataValue {
            content,
            content_len,
            entry_version,
        } = *c_repr;

        Ok(MDataValue {
            content: slice::from_raw_parts(content, content_len).to_vec(),
            entry_version,
        })
    }
}

/// Mutable data key.
#[derive(Hash, Eq, PartialEq, PartialOrd, Ord, Clone, Serialize, Deserialize, Debug)]
pub struct MDataKey {
    /// Key value.
    pub val: Vec<u8>,
}

impl MDataKey {
    /// Convert routing representation to `MDataKey`.
    pub fn from_routing(key: Vec<u8>) -> Self {
        MDataKey { val: key }
    }

    /// Returns FFI counterpart without consuming the object.
    pub fn as_repr_c(&self) -> ffi::MDataKey {
        ffi::MDataKey {
            val: self.val.as_ptr(),
            val_len: self.val.len(),
        }
    }
}

impl ReprC for MDataKey {
    type C = *const ffi::MDataKey;
    type Error = ();

    unsafe fn clone_from_repr_c(c_repr: Self::C) -> Result<Self, Self::Error> {
        let ffi::MDataKey { val, val_len } = *c_repr;

        Ok(MDataKey {
            val: slice::from_raw_parts(val, val_len).to_vec(),
        })
    }
}

/// Mutable data entry.
#[derive(Hash, Eq, PartialEq, PartialOrd, Ord, Clone, Serialize, Deserialize, Debug)]
pub struct MDataEntry {
    /// Key.
    pub key: MDataKey,
    /// Value.
    pub value: MDataValue,
}

impl MDataEntry {
    /// Returns FFI counterpart without consuming the object.
    pub fn as_repr_c(&self) -> ffi::MDataEntry {
        ffi::MDataEntry {
            key: self.key.as_repr_c(),
            value: self.value.as_repr_c(),
        }
    }
}

impl ReprC for MDataEntry {
    type C = *const ffi::MDataEntry;
    type Error = ();

    unsafe fn clone_from_repr_c(c_repr: Self::C) -> Result<Self, Self::Error> {
        let ffi::MDataEntry { key, value } = *c_repr;

        Ok(MDataEntry {
            key: MDataKey::clone_from_repr_c(&key)?,
            value: MDataValue::clone_from_repr_c(&value)?,
        })
    }
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use ffi_utils::ReprC;
    use ipc::BootstrapConfig;
    use routing::{XorName, XOR_NAME_LEN};
    use safe_crypto::{
        self, Nonce, PublicEncryptKey, PublicSignKey, SecretEncryptKey, SecretSignKey, SymmetricKey,
    };

    // Test converting an `AuthGranted` object to its FFI representation and then back again.
    #[test]
    fn auth_granted() {
        let (ok, _) = safe_crypto::gen_sign_keypair();
        let (pk, sk) = safe_crypto::gen_sign_keypair();
        let key = SymmetricKey::new();
        let (ourpk, oursk) = safe_crypto::gen_encrypt_keypair();
        let ak = AppKeys {
            owner_key: ok,
            enc_key: key,
            sign_pk: pk,
            sign_sk: sk,
            enc_pk: ourpk,
            enc_sk: oursk,
        };
        let ac = AccessContInfo {
            id: XorName([2; XOR_NAME_LEN]),
            tag: 681,
            nonce: Nonce::new(),
        };
        let ag = AuthGranted {
            app_keys: ak,
            bootstrap_config: BootstrapConfig::default(),
            access_container_info: ac,
            access_container_entry: AccessContainerEntry::default(),
        };

        let ffi = unwrap!(ag.into_repr_c());

        assert_eq!(ffi.access_container_info.tag, 681);

        let ag = unsafe { unwrap!(AuthGranted::clone_from_repr_c(&ffi)) };

        assert_eq!(ag.access_container_info.tag, 681);
    }

    // Testing converting an `AppKeys` object to its FFI representation and back again.
    #[test]
    fn app_keys() {
        let (ok, _) = safe_crypto::gen_sign_keypair();
        let (pk, sk) = safe_crypto::gen_sign_keypair();
        let key = SymmetricKey::new();
        let (ourpk, oursk) = safe_crypto::gen_encrypt_keypair();
        let ak = AppKeys {
            owner_key: ok,
            enc_key: key.clone(),
            sign_pk: pk,
            sign_sk: sk.clone(),
            enc_pk: ourpk,
            enc_sk: oursk.clone(),
        };

        let ffi_ak = ak.into_repr_c();

        assert_eq!(PublicSignKey::from_bytes(ffi_ak.owner_key), ok,);
        assert_eq!(SymmetricKey::from_bytes(ffi_ak.enc_key), key,);
        assert_eq!(PublicSignKey::from_bytes(ffi_ak.sign_pk), pk,);
        assert_eq!(SecretSignKey::from_bytes(ffi_ak.sign_sk), sk,);
        assert_eq!(PublicEncryptKey::from_bytes(ffi_ak.enc_pk), ourpk,);
        assert_eq!(SecretEncryptKey::from_bytes(ffi_ak.enc_sk), oursk,);

        let ak = unsafe { unwrap!(AppKeys::clone_from_repr_c(ffi_ak)) };

        assert_eq!(ak.owner_key, ok);
        assert_eq!(ak.enc_key, key);
        assert_eq!(ak.sign_pk, pk);
        assert_eq!(ak.sign_sk, sk);
        assert_eq!(ak.enc_pk, ourpk);
        assert_eq!(ak.enc_sk, oursk);
    }

    // Test converting an `AccessContInfo` struct to its FFI representation and back again.
    #[test]
    fn access_container() {
        let nonce = Nonce::new();
        let a = AccessContInfo {
            id: XorName([2; XOR_NAME_LEN]),
            tag: 681,
            nonce: nonce.clone(),
        };

        let ffi = a.into_repr_c();

        assert_eq!(ffi.id.iter().sum::<u8>() as usize, 2 * XOR_NAME_LEN);
        assert_eq!(ffi.tag, 681);
        assert_eq!(Nonce::from_bytes(ffi.nonce), nonce);

        let a = unsafe { unwrap!(AccessContInfo::clone_from_repr_c(ffi)) };

        assert_eq!(a.id.0.iter().sum::<u8>() as usize, 2 * XOR_NAME_LEN);
        assert_eq!(a.tag, 681);
        assert_eq!(a.nonce, nonce);
    }
}
