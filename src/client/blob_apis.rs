// Copyright 2021 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

use super::{
    blob_storage::{BlobStorage, BlobStorageDryRun},
    Client,
};
use crate::Error;
use bincode::{deserialize, serialize};
use log::{info, trace};
use self_encryption::{DataMap, SelfEncryptor};
use serde::{Deserialize, Serialize};
use sn_data_types::{Blob, BlobAddress, PrivateBlob, PublicBlob, PublicKey};
use sn_messaging::client::{BlobRead, BlobWrite, DataCmd, DataQuery, Query, QueryResponse};

#[derive(Serialize, Deserialize)]
enum DataMapLevel {
    // Holds the data map that is returned after writing the client's data
    // to the network
    Root(DataMap),
    // Holds the data map returned returned after a writing a
    // serialized blob that holds a non-root data map
    Child(DataMap),
}

impl Client {
    /// Read the contents of a blob from the network. The contents might be spread across
    /// different blobs in the network. This function invokes the self-encryptor and returns
    /// the data that was initially stored.
    ///
    /// Takes `position` and `len` arguments which specify the start position
    /// and the length of bytes to be read. Passing `None` to position reads the data from the beginning.
    /// Passing `None` to length reads the full length of the data.
    ///
    /// # Examples
    ///
    /// Get data
    ///
    /// ```no_run
    /// # extern crate tokio; use anyhow::Result;
    /// # use sn_client::utils::test_utils::read_network_conn_info;
    /// use sn_client::Client;
    /// use sn_data_types::BlobAddress;
    /// use xor_name::XorName;
    /// # #[tokio::main] async fn main() { let _: Result<()> = futures::executor::block_on( async {
    /// let target_blob = BlobAddress::Public(XorName::random());
    /// # let bootstrap_contacts = Some(read_network_conn_info()?);
    /// let client = Client::new(None, None, bootstrap_contacts).await?;
    ///
    /// // grab the random blob from the network
    /// let _data = client.read_blob(target_blob, None, None).await?;
    /// # Ok(())} );}
    /// ```
    pub async fn read_blob(
        &self,
        address: BlobAddress,
        position: Option<usize>,
        len: Option<usize>,
    ) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        trace!(
            "Fetch Blob: {:?} Position: {:?} Len: {:?}",
            &address,
            &position,
            &len
        );

        let data = self.fetch_blob_from_network(address).await?;
        let public = address.is_public();
        let data_map = self.unpack(data).await?;

        let raw_data = self
            .read_using_data_map(data_map, public, position, len)
            .await?;

        Ok(raw_data)
    }

    /// Store data in public blobs on the network.
    ///
    /// This performs self encrypt on the data itself and returns a single address using which the data can be read.
    /// It performs data storage as well as all necessary payment validation and checks against the client's AT2 actor.
    ///
    /// # Examples
    ///
    /// Store data
    ///
    /// ```no_run
    /// # extern crate tokio; use anyhow::Result;
    /// # use sn_client::utils::test_utils::read_network_conn_info;
    /// use sn_client::Client;
    /// use sn_data_types::Token;
    /// use std::str::FromStr;
    /// # #[tokio::main] async fn main() { let _: Result<()> = futures::executor::block_on( async {
    /// // Let's use an existing client, with a pre-existing balance to be used for write payments.
    /// # let bootstrap_contacts = Some(read_network_conn_info()?);
    /// let mut client = Client::new(None, None, bootstrap_contacts).await?;
    /// # let initial_balance = Token::from_str("100")?; client.trigger_simulated_farming_payout(initial_balance).await?;
    /// let data = b"some data".to_vec();
    /// // grab the random blob from the network
    /// let _address = client.store_public_blob(&data).await?;
    ///
    /// # let balance_after_write = client.get_local_balance().await; assert_ne!(initial_balance, balance_after_write); Ok(()) } ); }
    /// ```
    pub async fn store_public_blob(&self, data: &[u8]) -> Result<BlobAddress, Error> {
        self.create_new_blob(data, true).await
    }

    /// Store data in private blobs on the network.
    ///
    /// This performs self encrypt on the data itself and returns a single address using which the data can be read.
    /// It performs data storage as well as all necessary payment validation and checks against the client's AT2 actor.
    ///
    /// # Examples
    ///
    /// Store data
    ///
    /// ```no_run
    /// # extern crate tokio; use anyhow::Result;
    /// # use sn_client::utils::test_utils::read_network_conn_info;
    /// use sn_client::Client;
    /// use sn_data_types::Token;
    /// use std::str::FromStr;
    /// # #[tokio::main] async fn main() { let _: Result<()> = futures::executor::block_on( async {
    /// // Let's use an existing client, with a pre-existing balance to be used for write payments.
    /// # let bootstrap_contacts = Some(read_network_conn_info()?);
    /// let mut client = Client::new(None, None, bootstrap_contacts).await?;
    /// # let initial_balance = Token::from_str("100")?; client.trigger_simulated_farming_payout(initial_balance).await?;
    /// let data = b"some data".to_vec();
    /// // grab the random blob from the network
    /// let fetched_data = client.store_private_blob(&data).await?;
    ///
    /// println!( "{:?}", fetched_data ); // prints "some data"
    /// # let balance_after_write = client.get_local_balance().await; assert_ne!(initial_balance, balance_after_write); Ok(()) } ); }
    /// ```
    pub async fn store_private_blob(&self, data: &[u8]) -> Result<BlobAddress, Error> {
        self.create_new_blob(data, false).await
    }

    async fn create_new_blob(&self, data: &[u8], public: bool) -> Result<BlobAddress, Error> {
        let data_map = self.write_to_network(data, public).await?;

        let blob_content = serialize(&DataMapLevel::Root(data_map))?;
        let blob = self.pack(blob_content, public).await?;
        let blob_address = *blob.address();

        self.store_blob_on_network(blob).await?;

        Ok(blob_address)
    }

    pub(crate) async fn fetch_blob_from_network(
        &self,
        address: BlobAddress,
    ) -> Result<Blob, Error> {
        let res = self
            .send_query(Query::Data(DataQuery::Blob(BlobRead::Get(address))))
            .await?;
        let msg_id = res.msg_id;
        let data: Blob = match res.response {
            QueryResponse::GetBlob(result) => result.map_err(|err| Error::from((err, msg_id))),
            _ => return Err(Error::ReceivedUnexpectedEvent),
        }?;

        Ok(data)
    }

    pub(crate) async fn delete_blob_from_network(&self, address: BlobAddress) -> Result<(), Error> {
        let cmd = DataCmd::Blob(BlobWrite::DeletePrivate(address));
        self.pay_and_send_data_command(cmd).await?;

        Ok(())
    }

    // Private function that actually stores the given blob on the network.
    // Self Encryption is NOT APPLIED ON the blob that is passed to this function.
    // Clients should not call this function directly.
    pub(crate) async fn store_blob_on_network(&self, blob: Blob) -> Result<(), Error> {
        if !blob.validate_size() {
            return Err(Error::NetworkDataError(sn_data_types::Error::ExceededSize));
        }
        let cmd = DataCmd::Blob(BlobWrite::New(blob));
        self.pay_and_send_data_command(cmd).await?;
        Ok(())
    }

    /// Delete blob can only be performed on Private Blobs. But on those private blobs this will remove the data
    /// from the network.
    ///
    /// # Examples
    ///
    /// Remove data
    ///
    /// ```no_run
    /// # use sn_client::utils::test_utils::read_network_conn_info;
    /// use sn_client::Client;
    /// use sn_data_types::Token;
    /// use std::str::FromStr;
    /// # #[tokio::main] async fn main() { let _: anyhow::Result<()> = futures::executor::block_on( async {
    ///
    /// // Let's use an existing client, with a pre-existing balance to be used for write payments.
    /// # let bootstrap_contacts = Some(read_network_conn_info()?);
    /// let mut client = Client::new(None, None, bootstrap_contacts).await?;
    /// # let initial_balance = Token::from_str("100")?; client.trigger_simulated_farming_payout(initial_balance).await?;
    /// let data = b"some private data".to_vec();
    /// let address = client.store_private_blob(&data).await?;
    ///
    /// let _ = client.delete_blob(address).await?;
    ///
    /// // Now when we attempt to retrieve the blob, we should get an error
    ///
    /// match client.read_blob(address, None, None).await {
    ///     Err(error) => eprintln!("Expected error getting blob {:?}", error),
    ///     _ => return Err(anyhow::anyhow!("Should not have been able to retrieve this blob"))
    /// };
    /// #  Ok(())} );}
    /// ```
    pub async fn delete_blob(&self, address: BlobAddress) -> Result<(), Error> {
        info!("Deleting blob at given address: {:?}", address);

        let mut data = self.fetch_blob_from_network(address).await?;
        self.delete_blob_from_network(address).await?;

        loop {
            match deserialize(data.value())? {
                DataMapLevel::Root(data_map) => {
                    self.delete_using_data_map(data_map).await?;
                    return Ok(());
                }
                DataMapLevel::Child(data_map) => {
                    let serialized_blob = self
                        .read_using_data_map(data_map.clone(), false, None, None)
                        .await?;
                    self.delete_using_data_map(data_map).await?;
                    data = deserialize(&serialized_blob)?;
                }
            }
        }
    }

    /// Uses self_encryption to generate an encrypted Blob serialized data map,
    /// without connecting and/or writing to the network.
    pub async fn blob_data_map(
        mut data: Vec<u8>,
        privately_owned: Option<PublicKey>,
    ) -> Result<(DataMap, BlobAddress), Error> {
        // We generate a random public key as owner since this is used for dry-run only
        let mut is_original_data = true;

        let (data_map, blob) = loop {
            let blob_storage = BlobStorageDryRun::new(privately_owned);
            let self_encryptor =
                SelfEncryptor::new(blob_storage, DataMap::None).map_err(Error::SelfEncryption)?;
            self_encryptor
                .write(&data, 0)
                .await
                .map_err(Error::SelfEncryption)?;
            let (data_map, _) = self_encryptor
                .close()
                .await
                .map_err(Error::SelfEncryption)?;

            let blob_content = if is_original_data {
                is_original_data = false;
                serialize(&DataMapLevel::Root(data_map.clone()))?
            } else {
                serialize(&DataMapLevel::Child(data_map.clone()))?
            };

            let blob: Blob = if let Some(owner) = privately_owned {
                PrivateBlob::new(blob_content, owner).into()
            } else {
                PublicBlob::new(blob_content).into()
            };

            // If Blob (data map) is bigger than 1MB we need to break it down
            if blob.validate_size() {
                break (data_map, blob);
            } else {
                data = serialize(&blob)?;
            }
        };

        Ok((data_map, *blob.address()))
    }

    // --------------------------------------------
    // ---------- Private helpers -----------------
    // --------------------------------------------

    // Writes raw data to the network into immutable data chunks
    async fn write_to_network(&self, data: &[u8], public: bool) -> Result<DataMap, Error> {
        let blob_storage = BlobStorage::new(self.clone(), public);
        let self_encryptor = SelfEncryptor::new(blob_storage.clone(), DataMap::None)
            .map_err(Error::SelfEncryption)?;

        self_encryptor
            .write(data, 0)
            .await
            .map_err(Error::SelfEncryption)?;

        let (data_map, _) = self_encryptor
            .close()
            .await
            .map_err(Error::SelfEncryption)?;
        Ok(data_map)
    }

    // This function reads raw data from the network using the data map
    async fn read_using_data_map(
        &self,
        data_map: DataMap,
        public: bool,
        position: Option<usize>,
        len: Option<usize>,
    ) -> Result<Vec<u8>, Error> {
        let blob_storage = BlobStorage::new(self.clone(), public);
        let self_encryptor =
            SelfEncryptor::new(blob_storage, data_map).map_err(Error::SelfEncryption)?;

        let length = match len {
            None => self_encryptor.len().await,
            Some(request_length) => request_length,
        };

        let read_position = position.unwrap_or(0);

        match self_encryptor.read(read_position, length).await {
            Ok(data) => Ok(data),
            Err(error) => Err(Error::SelfEncryption(error)),
        }
    }

    async fn delete_using_data_map(&self, data_map: DataMap) -> Result<(), Error> {
        let blob_storage = BlobStorage::new(self.clone(), false);
        let self_encryptor =
            SelfEncryptor::new(blob_storage, data_map).map_err(Error::SelfEncryption)?;

        match self_encryptor.delete().await {
            Ok(_) => Ok(()),
            Err(error) => Err(Error::SelfEncryption(error)),
        }
    }

    /// Takes the "Root data map" and returns a Blob that is acceptable by the network
    ///
    /// If the root data map blob is too big, the whole blob is self-encrypted and the child data map is put into a blob.
    /// The above step is repeated as many times as required until the blob size is valid.
    async fn pack(&self, mut contents: Vec<u8>, public: bool) -> Result<Blob, Error> {
        loop {
            let data: Blob = if public {
                PublicBlob::new(contents).into()
            } else {
                PrivateBlob::new(contents, self.public_key()).into()
            };

            // If data map blob is less thatn 1MB return it so it can be directly sent to the network
            if data.validate_size() {
                return Ok(data);
            } else {
                let serialized_blob = serialize(&data)?;
                let data_map = self.write_to_network(&serialized_blob, public).await?;
                contents = serialize(&DataMapLevel::Child(data_map))?;
            }
        }
    }

    /// Takes a blob and fetches the data map from it.
    /// If the data map is not the root data map of the user's contents,
    /// the process repeats itself until it obtains the root data map.
    async fn unpack(&self, mut data: Blob) -> Result<DataMap, Error> {
        loop {
            let public = data.is_public();
            match deserialize(data.value())? {
                DataMapLevel::Root(data_map) => {
                    return Ok(data_map);
                }
                DataMapLevel::Child(data_map) => {
                    let serialized_blob = self
                        .read_using_data_map(data_map, public, None, None)
                        .await?;
                    data = deserialize(&serialized_blob)?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Blob, BlobAddress, Client, DataMap, DataMapLevel, Error};
    use crate::utils::{generate_random_vector, test_utils::create_test_client};
    use crate::{client::blob_storage::BlobStorage, retry_err_loop, retry_loop};
    use anyhow::{anyhow, bail, Result};
    use bincode::deserialize;
    use self_encryption::Storage;
    use sn_data_types::{PrivateBlob, PublicBlob, Token};
    use sn_messaging::client::Error as ErrorMessage;
    use std::str::FromStr;

    // Test putting and getting pub Blob.
    #[tokio::test]
    pub async fn public_blob_test() -> Result<()> {
        let client = create_test_client().await?;
        let value = generate_random_vector::<u8>(10);
        let (_, expected_address) = Client::blob_data_map(value.clone(), None).await?;

        // Get non-existent Blob
        match client.read_blob(expected_address, None, None).await {
            Ok(data) => bail!("Public Blob should not exist yet: {:?}", data),
            Err(Error::ErrorMessage {
                source: ErrorMessage::DataNotFound(_),
                ..
            }) => (),
            Err(e) => bail!("Unexpected: {:?}", e),
        }

        // Put blob
        let pub_address = client.store_public_blob(&value).await?;
        // check it's the expected Blob address
        assert_eq!(expected_address, pub_address);

        // Assert that the blob was written
        let fetched_data = retry_loop!(client.read_blob(pub_address, None, None));
        assert_eq!(value, fetched_data);

        // Test putting public Blob with the same value.
        // Should not conflict and return same address
        let addr = client.store_public_blob(&value).await?;
        assert_eq!(addr, pub_address);

        Ok(())
    }

    // Test putting, getting, and deleting private blob.
    #[tokio::test]
    pub async fn private_blob_test() -> Result<()> {
        let client = create_test_client().await?;

        let value = generate_random_vector::<u8>(10);

        let owner = client.public_key();
        let (_, expected_address) = Client::blob_data_map(value.clone(), Some(owner)).await?;

        // Get non-existent Blob
        match client.read_blob(expected_address, None, None).await {
            Ok(_) => bail!("Private Blob should not exist yet"),
            Err(Error::ErrorMessage {
                source: ErrorMessage::DataNotFound(_),
                ..
            }) => (),
            Err(e) => bail!("Expecting DataNotFound error, got: {:?}", e),
        }

        // Put Blob
        let priv_address = client.store_private_blob(&value).await?;
        // check it's the expected Blob address
        assert_eq!(expected_address, priv_address);

        // Assert that the blob is stored.
        let fetched_data = retry_loop!(client.read_blob(priv_address, None, None));
        assert_eq!(value, fetched_data);

        // Test putting private Blob with the same value.
        // Should not conflict and return same address
        let addr = client.store_private_blob(&value).await?;
        assert_eq!(addr, priv_address);

        // Test putting public Blob with the same value. Should not conflict.
        let pub_address = client.store_public_blob(&value).await?;

        // Assert that the pub Blob is stored.
        let fetched_data = retry_loop!(client.read_blob(pub_address, None, None));
        assert_eq!(value, fetched_data);

        // Delete Blob
        client.delete_blob(priv_address).await?;

        // Make sure Blob was deleted
        let mut attempts = 10u8;
        while client.read_blob(priv_address, None, None).await.is_ok() {
            tokio::time::sleep(tokio::time::Duration::from_millis(4000)).await;
            if attempts == 0 {
                bail!("The private Blob was not deleted: {:?}", priv_address);
            } else {
                attempts -= 1;
            }
        }

        // Test putting private Blob with the same value again. Should not conflict.
        let new_addr = client.store_private_blob(&value).await?;
        assert_eq!(new_addr, priv_address);

        // Assert that the Blob is stored again.
        let fetched_data = retry_loop!(client.read_blob(priv_address, None, None));
        assert_eq!(value, fetched_data);

        Ok(())
    }

    #[tokio::test]
    pub async fn private_delete_large() -> Result<()> {
        let client = create_test_client().await?;

        let value = generate_random_vector::<u8>(1024 * 1024);
        let address = client.store_private_blob(&value).await?;

        // let's make sure we have all chunks stored on the network
        let _ = retry_loop!(client.read_blob(address, None, None));

        let fetched_data = retry_loop!(client.fetch_blob_from_network(address));

        let root_data_map = match deserialize(fetched_data.value())? {
            DataMapLevel::Root(data_map) => data_map,
            DataMapLevel::Child(data_map) => bail!(
                "A DataMapLevel::Child data-map was unexpectedly returned: {:?}",
                data_map
            ),
        };

        client.delete_blob(address).await?;

        let mut blob_storage = BlobStorage::new(client, false);

        if let DataMap::Chunks(chunks) = root_data_map {
            for chunk in chunks {
                let _ = retry_err_loop!(blob_storage.get(&chunk.hash));
            }

            Ok(())
        } else {
            Err(anyhow!(
                "It didn't return DataMap::Chunks, instead: {:?}",
                root_data_map
            ))
        }
    }

    #[tokio::test]
    pub async fn blob_deletions_should_cost_put_price() -> Result<()> {
        let client = create_test_client().await?;

        let address = client
            .store_private_blob(&generate_random_vector::<u8>(10))
            .await?;

        let _ = retry_loop!(client.read_blob(address, None, None));

        let balance_before_delete = client.get_balance().await?;
        retry_loop!(client.delete_blob(address));
        let new_balance = client.get_balance().await?;

        // make sure we have _some_ balance
        assert_ne!(balance_before_delete, Token::from_str("0")?);
        assert_ne!(balance_before_delete, new_balance);

        Ok(())
    }

    // Test creating and retrieving a 1kb blob.
    #[tokio::test]
    pub async fn create_and_retrieve_1kb_pub_unencrypted() -> Result<()> {
        let size = 1024;

        gen_data_then_create_and_retrieve(size, true).await?;

        Ok(())
    }

    #[tokio::test]
    pub async fn create_and_retrieve_1kb_private_unencrypted() -> Result<()> {
        let size = 1024;

        gen_data_then_create_and_retrieve(size, false).await?;
        Ok(())
    }

    #[tokio::test]
    pub async fn create_and_retrieve_1kb_put_pub_retrieve_private() -> Result<()> {
        let size = 1024;
        let data = generate_random_vector(size);

        let client = create_test_client().await?;
        let address = client.store_public_blob(&data).await?;

        // let's make sure the public Blob is stored
        let _ = retry_loop!(client.read_blob(address, None, None));

        // and now trying to read a private Blob with same address should fail
        let res = client
            .read_blob(BlobAddress::Private(*address.name()), None, None)
            .await;
        assert!(res.is_err());

        Ok(())
    }

    #[tokio::test]
    pub async fn create_and_retrieve_1kb_put_private_retrieve_pub() -> Result<()> {
        let size = 1024;

        let value = generate_random_vector(size);

        let client = create_test_client().await?;

        let address = client.store_private_blob(&value).await?;

        // let's make sure the private Blob is stored
        let _ = retry_loop!(client.read_blob(address, None, None));

        // and now trying to read a public Blob with same address should fail
        let res = client
            .read_blob(BlobAddress::Public(*address.name()), None, None)
            .await;
        assert!(res.is_err());

        Ok(())
    }

    #[tokio::test]
    pub async fn create_and_retrieve_1mb_public() -> Result<()> {
        let size = 1024 * 1024;
        gen_data_then_create_and_retrieve(size, true).await?;
        Ok(())
    }

    #[tokio::test]
    pub async fn create_and_retrieve_1mb_private() -> Result<()> {
        let size = 1024 * 1024;
        gen_data_then_create_and_retrieve(size, false).await?;
        Ok(())
    }

    // ----------------------------------------------------------------
    // 10mb (ie. more than 1 chunk)
    // ----------------------------------------------------------------
    #[tokio::test]
    pub async fn create_and_retrieve_10mb_private() -> Result<()> {
        let size = 1024 * 1024 * 10;
        gen_data_then_create_and_retrieve(size, false).await?;

        Ok(())
    }

    #[tokio::test]
    pub async fn create_and_retrieve_10mb_public() -> Result<()> {
        let size = 1024 * 1024 * 10;
        gen_data_then_create_and_retrieve(size, true).await?;
        Ok(())
    }

    #[tokio::test]
    pub async fn create_and_retrieve_index_based() -> Result<()> {
        create_and_index_based_retrieve(1024).await
    }

    async fn create_and_index_based_retrieve(size: usize) -> Result<()> {
        // Test read first half
        let data = generate_random_vector(size);
        let client = create_test_client().await?;

        let address = client.store_public_blob(&data).await?;

        let fetched_data = retry_loop!(client.read_blob(address, None, Some(size / 2)));
        assert_eq!(fetched_data, data[0..size / 2].to_vec());

        // Test read second half
        let data = generate_random_vector(size);
        let client = create_test_client().await?;

        let address = client.store_public_blob(&data).await?;

        let fetched_data = retry_loop!(client.read_blob(address, Some(size / 2), Some(size / 2)));
        assert_eq!(fetched_data, data[size / 2..size].to_vec());

        Ok(())
    }

    #[allow(clippy::match_wild_err_arm)]
    async fn gen_data_then_create_and_retrieve(size: usize, public: bool) -> Result<()> {
        let raw_data = generate_random_vector(size);

        let client = create_test_client().await?;

        // gen address without putting to the network (public and unencrypted)
        let blob = if public {
            Blob::Public(PublicBlob::new(raw_data.clone()))
        } else {
            Blob::Private(PrivateBlob::new(raw_data.clone(), client.public_key()))
        };

        let address_before = blob.address();

        // attempt to retrieve it with generated address (it should error)
        let res = client.read_blob(*address_before, None, None).await;
        match res {
            Err(Error::ErrorMessage {
                source: ErrorMessage::DataNotFound(_),
                ..
            }) => (),
            Ok(_) => bail!("Blob unexpectedly retrieved using address generated by gen_data_map"),
            Err(_) => bail!(
                "Unexpected error when Blob retrieved using address generated by gen_data_map"
            ),
        };

        let address = if public {
            client.store_public_blob(&raw_data).await?
        } else {
            client.store_private_blob(&raw_data).await?
        };

        // now that it was put to the network we should be able to retrieve it
        let fetched_data = retry_loop!(client.read_blob(address, None, None));
        // then the content should be what we put
        assert_eq!(fetched_data, raw_data);

        // now let's test Blob data map generation utility returns the correct Blob address
        let privately_owned = if public {
            None
        } else {
            Some(client.public_key())
        };
        let (_, blob_address) = Client::blob_data_map(raw_data, privately_owned).await?;
        assert_eq!(blob_address, address);

        Ok(())
    }
}
