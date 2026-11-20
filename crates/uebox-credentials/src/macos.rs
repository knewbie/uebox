use std::ptr;

use core_foundation::{
    array::CFArray,
    base::{CFType, CFTypeRef, TCFType},
    boolean::CFBoolean,
    data::CFData,
    dictionary::{CFDictionary, CFDictionaryRef},
    string::{CFString, CFStringRef},
};

use crate::{CredentialBackend, CredentialError, CredentialRef, Secret, MAX_SECRET_BYTES};

// Security.framework's generic-password API. Types match the macOS SDK headers.
#[allow(non_upper_case_globals)]
#[link(name = "Security", kind = "framework")]
extern "C" {
    static kSecClass: CFStringRef;
    static kSecClassGenericPassword: CFStringRef;
    static kSecAttrService: CFStringRef;
    static kSecAttrAccount: CFStringRef;
    static kSecUseKeychain: CFStringRef;
    static kSecMatchSearchList: CFStringRef;
    static kSecReturnData: CFStringRef;
    static kSecValueData: CFStringRef;

    fn SecKeychainCopyDefault(keychain: *mut CFTypeRef) -> i32;
    fn SecItemAdd(attributes: CFDictionaryRef, result: *mut CFTypeRef) -> i32;
    fn SecItemCopyMatching(query: CFDictionaryRef, result: *mut CFTypeRef) -> i32;
    fn SecItemUpdate(query: CFDictionaryRef, attributes: CFDictionaryRef) -> i32;
    fn SecItemDelete(query: CFDictionaryRef) -> i32;
}

const SUCCESS: i32 = 0;
const DUPLICATE_ITEM: i32 = -25299;
const ITEM_NOT_FOUND: i32 = -25300;

pub(super) struct NativeBackend;

fn error(status: i32) -> CredentialError {
    match status {
        // No keychain available, no such keychain, no default keychain.
        -25291 | -25294 | -25307 => CredentialError::Unavailable,
        // Cancelled, missing entitlement, read-only, auth failed, interaction prohibited.
        -128 | -34018 | -25292 | -25293 | -25308 => CredentialError::AccessDenied,
        _ => CredentialError::BackendFailure,
    }
}

fn found(status: i32) -> Result<bool, CredentialError> {
    match status {
        SUCCESS => Ok(true),
        ITEM_NOT_FOUND => Ok(false),
        _ => Err(error(status)),
    }
}

fn take_owned(result: CFTypeRef) -> Option<CFType> {
    // A non-null result from SecKeychainCopyDefault or SecItemCopyMatching is owned by the caller.
    if result.is_null() {
        None
    } else {
        Some(unsafe { CFType::wrap_under_create_rule(result) })
    }
}

fn default_keychain() -> Result<CFType, CredentialError> {
    let mut keychain = ptr::null();
    let status = unsafe { SecKeychainCopyDefault(&mut keychain) };
    let keychain = take_owned(keychain);
    if status != SUCCESS {
        return Err(error(status));
    }
    keychain.ok_or(CredentialError::Unavailable)
}

fn item_attributes(reference: &CredentialRef) -> Vec<(CFString, CFType)> {
    // Security.framework provides these process-lifetime CFString constants.
    unsafe {
        vec![
            (
                CFString::wrap_under_get_rule(kSecClass),
                CFString::wrap_under_get_rule(kSecClassGenericPassword).into_CFType(),
            ),
            (
                CFString::wrap_under_get_rule(kSecAttrService),
                CFString::new(&reference.service()).into_CFType(),
            ),
            (
                CFString::wrap_under_get_rule(kSecAttrAccount),
                CFString::new(reference.id()).into_CFType(),
            ),
        ]
    }
}

fn search_attributes(reference: &CredentialRef, keychain: &CFType) -> Vec<(CFString, CFType)> {
    let mut query = item_attributes(reference);
    query.push((
        unsafe { CFString::wrap_under_get_rule(kSecMatchSearchList) },
        CFArray::from_CFTypes(std::slice::from_ref(keychain)).into_CFType(),
    ));
    query
}

impl CredentialBackend for NativeBackend {
    fn set(&self, reference: &CredentialRef, secret: &Secret) -> Result<(), CredentialError> {
        let keychain = default_keychain()?;
        let value = (
            unsafe { CFString::wrap_under_get_rule(kSecValueData) },
            CFData::from_buffer(secret.expose()).into_CFType(),
        );
        let mut add = item_attributes(reference);
        add.push((
            unsafe { CFString::wrap_under_get_rule(kSecUseKeychain) },
            keychain.clone(),
        ));
        add.push(value.clone());
        let add = CFDictionary::from_CFType_pairs(&add);
        let query = CFDictionary::from_CFType_pairs(&search_attributes(reference, &keychain));
        let changes = CFDictionary::from_CFType_pairs(&[value]);

        // An in-place update preserves the existing item's access controls. Retry once if
        // another client deletes the item between a duplicate response and the update.
        for _ in 0..2 {
            match unsafe { SecItemAdd(add.as_concrete_TypeRef(), ptr::null_mut()) } {
                SUCCESS => return Ok(()),
                DUPLICATE_ITEM => {}
                status => return Err(error(status)),
            }
            match unsafe {
                SecItemUpdate(query.as_concrete_TypeRef(), changes.as_concrete_TypeRef())
            } {
                SUCCESS => return Ok(()),
                ITEM_NOT_FOUND => {}
                status => return Err(error(status)),
            }
        }
        Err(CredentialError::BackendFailure)
    }

    fn get(&self, reference: &CredentialRef) -> Result<Option<Secret>, CredentialError> {
        let mut query = search_attributes(reference, &default_keychain()?);
        query.push((
            unsafe { CFString::wrap_under_get_rule(kSecReturnData) },
            CFBoolean::true_value().into_CFType(),
        ));
        let query = CFDictionary::from_CFType_pairs(&query);
        let mut result = ptr::null();
        let status = unsafe { SecItemCopyMatching(query.as_concrete_TypeRef(), &mut result) };
        let result = take_owned(result);
        if !found(status)? {
            return Ok(None);
        }
        let data = result
            .and_then(CFType::downcast_into::<CFData>)
            .ok_or(CredentialError::BackendFailure)?;
        if !(1..=MAX_SECRET_BYTES as isize).contains(&data.len()) {
            return Err(CredentialError::BackendFailure);
        }
        Secret::new(data.bytes().to_vec())
            .map(Some)
            .map_err(|_| CredentialError::BackendFailure)
    }

    fn delete(&self, reference: &CredentialRef) -> Result<bool, CredentialError> {
        let query =
            CFDictionary::from_CFType_pairs(&search_attributes(reference, &default_keychain()?));
        found(unsafe { SecItemDelete(query.as_concrete_TypeRef()) })
    }

    fn has(&self, reference: &CredentialRef) -> Result<bool, CredentialError> {
        let query =
            CFDictionary::from_CFType_pairs(&search_attributes(reference, &default_keychain()?));
        found(unsafe { SecItemCopyMatching(query.as_concrete_TypeRef(), ptr::null_mut()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_native_status_without_a_keychain_call() {
        assert_eq!(found(ITEM_NOT_FOUND), Ok(false));
        assert_eq!(found(-25307), Err(CredentialError::Unavailable));
        assert_eq!(found(-25308), Err(CredentialError::AccessDenied));
        assert_eq!(found(-50), Err(CredentialError::BackendFailure));
    }
}
