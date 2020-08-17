// Copyright 2018 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

use crate::client::{COST_OF_PUT};




use safe_nd::{ClientFullId, Keypair, Money};



use unwrap::unwrap;

/// Generates a random BLS secret and public keypair.
pub fn gen_bls_keypair() -> Keypair {
    let mut rng = rand::thread_rng();
    Keypair::new_bls(&mut rng)
}

/// Generates a random client full ID.
pub fn gen_client_id() -> ClientFullId {
    let mut rng = rand::thread_rng();
    ClientFullId::new_bls(&mut rng)
}

/// Helper function to calculate the total cost of expenditure by adding number of mutations and
/// amount of transferred coins if any.
pub fn calculate_new_balance(
    mut balance: Money,
    mutation_count: Option<u64>,
    transferred_coins: Option<Money>,
) -> Money {
    if let Some(x) = mutation_count {
        balance = unwrap!(balance.checked_sub(Money::from_nano(x * COST_OF_PUT.as_nano())));
    }
    if let Some(coins) = transferred_coins {
        balance = unwrap!(balance.checked_sub(coins));
    }

    // #[cfg(feature = "testing")]
    // {
    //     // add on our 10 coin starter balance in testing
    //     balance = unwrap!(balance.checked_add(Money::from_str("10").unwrap()))
    // }
    balance
}

/// Initialises `env_logger` with custom settings.
pub fn init_log() {
    use std::io::Write;
    let do_format = move |formatter: &mut env_logger::fmt::Formatter, record: &log::Record<'_>| {
        let now = formatter.timestamp();
        writeln!(
            formatter,
            "{} {} [{}:{}] {}",
            formatter.default_styled_level(record.level()),
            now,
            record.file().unwrap_or_default(),
            record.line().unwrap_or_default(),
            record.args()
        )
    };
    let _ = env_logger::Builder::from_default_env()
        .format(do_format)
        .is_test(true)
        .try_init();
}