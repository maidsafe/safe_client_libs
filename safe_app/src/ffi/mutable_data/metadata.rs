// Copyright 2018 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under the MIT license <LICENSE-MIT
// https://opensource.org/licenses/MIT> or the Modified BSD license <LICENSE-BSD
// https://opensource.org/licenses/BSD-3-Clause>, at your option. This file may not be copied,
// modified, or distributed except according to those terms. Please review the Licences for the
// specific language governing permissions and limitations relating to use of the SAFE Network
// Software.

//! FFI routines for handling mutable data metadata.

use crate::ffi::errors::Result;
use bincode::serialize;
use ffi_utils::{catch_unwind_cb, FfiResult, ReprC, FFI_RESULT_OK};
use safe_core::ffi::ipc::resp::MetadataResponse;
use safe_core::core_structs::UserMetadata;
use std::os::raw::c_void;

/// Serialize metadata.
#[no_mangle]
pub unsafe extern "C" fn mdata_encode_metadata(
    metadata: *const MetadataResponse,
    user_data: *mut c_void,
    o_cb: extern "C" fn(
        user_data: *mut c_void,
        result: *const FfiResult,
        encoded: *const u8,
        encoded_len: usize,
    ),
) {
    catch_unwind_cb(user_data, o_cb, || -> Result<_> {
        let metadata = UserMetadata::clone_from_repr_c(metadata)?;
        let encoded = serialize(&metadata)?;
        o_cb(user_data, FFI_RESULT_OK, encoded.as_ptr(), encoded.len());
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use crate::ffi::mutable_data::metadata::mdata_encode_metadata;
    use bincode::deserialize;
    use ffi_utils::test_utils::call_vec_u8;
    use safe_core::core_structs::UserMetadata;

    // Test serializing and deserializing metadata.
    #[test]
    fn serialize_metadata() {
        let metadata1 = UserMetadata {
            name: None,
            description: Some(String::from("test")),
        };

        let metadata_resp = match metadata1.clone().into_md_response(Default::default(), 0) {
            Ok(val) => val,
            _ => panic!("An error occurred"),
        };

        let serialised = unsafe {
            unwrap!(call_vec_u8(|ud, cb| mdata_encode_metadata(
                &metadata_resp,
                ud,
                cb
            ),))
        };

        let metadata2 = unwrap!(deserialize::<UserMetadata>(&serialised));

        assert_eq!(metadata1, metadata2);
    }
}
