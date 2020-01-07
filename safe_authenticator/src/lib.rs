// Copyright 2018 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

//! SAFE Authenticator

#![doc(
    html_logo_url = "https://raw.githubusercontent.com/maidsafe/QA/master/Images/maidsafe_logo.png",
    html_favicon_url = "http://maidsafe.net/img/favicon.ico",
    test(attr(forbid(warnings)))
)]
// For explanation of lint checks, run `rustc -W help`.
#![deny(unsafe_code)]
#![warn(
    missing_docs,
    trivial_casts,
    trivial_numeric_casts,
    unused_extern_crates,
    unused_import_braces,
    unused_qualifications,
    unused_results
)]
#![allow(
    // Our unsafe FFI functions are missing safety documentation. It is probably not necessary for
    // us to provide this for every single function as that would be repetitive and verbose.
    clippy::missing_safety_doc,
)]

#[macro_use]
extern crate ffi_utils;
#[macro_use]
extern crate log;
#[macro_use]
extern crate safe_core;
#[macro_use]
extern crate unwrap;

// Public exports. See https://github.com/maidsafe/safe_client_libs/wiki/Export-strategy.

// Export FFI interface.

pub use crate::ffi::apps::*;
pub use crate::ffi::errors::codes::*;
pub use crate::ffi::ipc::*;
pub use crate::ffi::logging::*;
pub use crate::ffi::*;

// Export public auth interface.

pub use self::errors::AuthError;
pub use client::AuthClient;

pub mod access_container;
pub mod app_auth;
pub mod app_container;
pub mod apps;
pub mod config;
pub mod errors;
pub mod ffi;
pub mod ipc;
pub mod revocation;
#[cfg(any(test, feature = "testing"))]
#[macro_use]
pub mod test_utils;

mod client;
mod std_dirs;
#[cfg(test)]
mod tests;

use crate::ffi::errors::Error;
use futures::stream::Stream;
use futures::sync::mpsc;
use futures::{Future, IntoFuture};
#[cfg(any(test, feature = "testing"))]
use safe_core::utils::test_utils::gen_client_id;
#[cfg(feature = "mock-network")]
use safe_core::ConnectionManager;
use safe_core::{event_loop, CoreMsg, CoreMsgTx, FutureExt, NetworkEvent, NetworkTx};
use safe_nd::ClientFullId;
use std::sync::mpsc as std_mpsc;
use std::sync::mpsc::sync_channel;
use std::sync::Mutex;
use std::thread::JoinHandle;
use tokio::runtime::current_thread::{Handle, Runtime};

/// Future type specialised with `AuthError` as an error type.
pub type AuthFuture<T> = dyn Future<Item = T, Error = AuthError>;
/// Transmitter of `AuthClient` messages.
pub type AuthMsgTx = CoreMsgTx<AuthClient, ()>;

macro_rules! try_tx {
    ($result:expr, $tx:ident) => {
        match $result {
            Ok(res) => res,
            Err(e) => {
                return unwrap!($tx.send(Err((None, AuthError::from(e)))));
            }
        }
    };
}

/// Authenticator instance.
pub struct Authenticator {
    /// Channel to communicate with the core event loop.
    pub core_tx: Mutex<AuthMsgTx>,
    _core_joiner: JoinHandle<()>,
}

impl Authenticator {
    /// Send a message to the authenticator event loop.
    pub fn send<F>(&self, f: F) -> crate::ffi::errors::Result<()>
    where
        F: FnOnce(&AuthClient) -> Option<Box<dyn Future<Item = (), Error = ()>>> + Send + 'static,
    {
        let msg = CoreMsg::new(|client, _| f(client));
        let core_tx = unwrap!(self.core_tx.lock());
        core_tx.unbounded_send(msg).map_err(Error::from)
    }

    /// Create a new account.
    pub fn create_acc<S, N>(
        locator: S,
        password: S,
        client_id: ClientFullId,
        disconnect_notifier: N,
    ) -> Result<Self, AuthError>
    where
        N: FnMut() + Send + 'static,
        S: Into<String>,
    {
        let locator = locator.into();
        let password = password.into();

        Self::create_acc_impl(
            move |el_h, core_tx, net_tx| {
                AuthClient::registered(&locator, &password, client_id, el_h, core_tx, net_tx)
            },
            disconnect_notifier,
        )
    }

    /// Create a new account.
    fn create_acc_impl<F: 'static + Send, N>(
        create_client_fn: F,
        mut disconnect_notifier: N,
    ) -> Result<Self, AuthError>
    where
        N: FnMut() + Send + 'static,
        F: FnOnce(Handle, AuthMsgTx, NetworkTx) -> Result<AuthClient, AuthError>,
    {
        let (tx, rx) = sync_channel(0);

        let joiner = std::thread::Builder::new()
            .name(String::from("Core Event Loop"))
            .spawn(move || {
                let mut el = try_tx!(Runtime::new(), tx);
                let el_h = el.handle();

                let (core_tx, core_rx) = mpsc::unbounded();
                let core_tx2 = core_tx.clone();
                let (net_tx, net_rx) = mpsc::unbounded::<NetworkEvent>();

                let net_obs_fut = net_rx
                    .then(move |net_event| {
                        if let Ok(NetworkEvent::Disconnected) = net_event {
                            disconnect_notifier();
                        }
                        ok!(())
                    })
                    .for_each(|_| Ok(()));
                let _ = el.spawn(net_obs_fut);

                let client = try_tx!(create_client_fn(el_h, core_tx.clone(), net_tx), tx);

                unwrap!(
                    core_tx.unbounded_send(CoreMsg::new(move |client, &()| std_dirs::create(
                        client
                    )
                    .map_err(|error| AuthError::AccountContainersCreation(error.to_string()))
                    .then(move |res| {
                        match res {
                            Ok(_) => unwrap!(tx.send(Ok(core_tx2))),
                            Err(error) => unwrap!(tx.send(Err((Some(core_tx2), error)))),
                        }

                        Ok(())
                    })
                    .into_box()
                    .into()))
                );

                event_loop::run(el, &client, &(), core_rx);
            })
            .map_err(AuthError::from)?;

        let core_tx = match rx.recv()? {
            Ok(core_tx) => core_tx,
            Err((None, e)) => return Err(e),
            Err((Some(core_tx), e)) => {
                // Make sure to shut down the event loop
                core_tx.unbounded_send(CoreMsg::build_terminator())?;
                return Err(e);
            }
        };

        Ok(Self {
            core_tx: Mutex::new(core_tx),
            _core_joiner: joiner,
        })
    }

    /// Log in to an existing account
    pub fn login<S, N>(locator: S, password: S, disconnect_notifier: N) -> Result<Self, AuthError>
    where
        S: Into<String>,
        N: FnMut() + Send + 'static,
    {
        let locator = locator.into();
        let password = password.into();

        Self::login_impl(
            move |el_h, core_tx, net_tx| {
                AuthClient::login(&locator, &password, el_h, core_tx, net_tx)
            },
            disconnect_notifier,
        )
    }

    /// Log in to an existing account.
    pub fn login_impl<F: Send + 'static, N>(
        create_client_fn: F,
        mut disconnect_notifier: N,
    ) -> Result<Self, AuthError>
    where
        F: FnOnce(Handle, AuthMsgTx, NetworkTx) -> Result<AuthClient, AuthError>,
        N: FnMut() + Send + 'static,
    {
        let (tx, rx) = sync_channel(0);

        let joiner = std::thread::Builder::new()
            .name(String::from("Core Event Loop"))
            .spawn(move || {
                let mut el = try_tx!(Runtime::new(), tx);
                let el_h = el.handle();

                let (core_tx, core_rx) = mpsc::unbounded();
                let (net_tx, net_rx) = mpsc::unbounded::<NetworkEvent>();
                let core_tx_clone = core_tx.clone();

                let net_obs_fut = net_rx
                    .then(move |net_event| {
                        if let Ok(NetworkEvent::Disconnected) = net_event {
                            disconnect_notifier();
                        }
                        ok!(())
                    })
                    .for_each(|_| Ok(()));
                let _ = el.spawn(net_obs_fut);

                let client = try_tx!(create_client_fn(el_h, core_tx_clone, net_tx), tx);

                if client.std_dirs_created() {
                    unwrap!(tx.send(Ok(core_tx)));
                } else {
                    // Standard directories haven't been created during
                    // the user account registration - retry it again.
                    let tx2 = tx.clone();
                    let core_tx2 = core_tx.clone();
                    let core_tx3 = core_tx.clone();

                    unwrap!(core_tx.unbounded_send(CoreMsg::new(move |client, &()| {
                        std_dirs::create(client)
                            .map(move |()| {
                                unwrap!(tx.send(Ok(core_tx2)));
                            })
                            .map_err(move |e| {
                                unwrap!(tx2.send(Err((Some(core_tx3), e))));
                            })
                            .into_box()
                            .into()
                    })));
                }

                event_loop::run(el, &client, &(), core_rx);
            })
            .map_err(AuthError::from)?;

        let core_tx = match rx.recv()? {
            Ok(core_tx) => core_tx,
            Err((None, e)) => return Err(e),
            Err((Some(core_tx), e)) => {
                // Make sure to shut down the event loop
                core_tx.unbounded_send(CoreMsg::build_terminator())?;
                return Err(e);
            }
        };

        Ok(Self {
            core_tx: Mutex::new(core_tx),
            _core_joiner: joiner,
        })
    }
}

/// Run the given closure inside the event loop of the authenticator. The closure
/// should return a future which will then be driven to completion and its result
/// returned.
pub fn run<F, I, T>(authenticator: &Authenticator, f: F) -> Result<T, AuthError>
where
    F: FnOnce(&AuthClient) -> I + Send + 'static,
    I: IntoFuture<Item = T, Error = AuthError> + 'static,
    T: Send + 'static,
{
    let (tx, rx) = std_mpsc::channel();

    unwrap!(authenticator.send(move |client| {
        let future = f(client)
            .into_future()
            .then(move |result| {
                unwrap!(tx.send(result));
                Ok(())
            })
            .into_box();

        Some(future)
    }));

    unwrap!(rx.recv())
}

#[cfg(any(test, feature = "testing"))]
impl Authenticator {
    /// Create a new account with given seed.
    pub fn create_acc_with_seed<S, N>(seed: S, disconnect_notifier: N) -> Result<Self, AuthError>
    where
        S: Into<String>,
        N: FnMut() + Send + 'static,
    {
        let seed = seed.into();
        let client_id = gen_client_id();
        Self::login_impl(
            move |el_h, core_tx, net_tx| {
                AuthClient::registered_with_seed(&seed, client_id, el_h, core_tx, net_tx)
            },
            disconnect_notifier,
        )
    }

    /// Login to an existing account using the same seed that was used during account creation.
    pub fn login_with_seed<S, N>(seed: S, disconnect_notifier: N) -> Result<Self, AuthError>
    where
        S: Into<String>,
        N: FnMut() + Send + 'static,
    {
        let seed = seed.into();
        Self::login_impl(
            move |el_h, core_tx, net_tx| AuthClient::login_with_seed(&seed, el_h, core_tx, net_tx),
            disconnect_notifier,
        )
    }
}

#[cfg(feature = "mock-network")]
impl Authenticator {
    #[allow(unused)]
    fn create_acc_with_hook<F, S, N>(
        locator: S,
        password: S,
        client_id: ClientFullId,
        disconnect_notifier: N,
        connection_manager_wrapper_fn: F,
    ) -> Result<Self, AuthError>
    where
        N: FnMut() + Send + 'static,
        F: Fn(ConnectionManager) -> ConnectionManager + Send + 'static,
        S: Into<String>,
    {
        let locator = locator.into();
        let password = password.into();

        Self::create_acc_impl(
            move |el_h, core_tx_clone, net_tx| {
                AuthClient::registered_with_hook(
                    &locator,
                    &password,
                    client_id,
                    el_h,
                    core_tx_clone,
                    net_tx,
                    connection_manager_wrapper_fn,
                )
            },
            disconnect_notifier,
        )
    }

    #[allow(unused)]
    fn login_with_hook<F, S, N>(
        locator: S,
        password: S,
        disconnect_notifier: N,
        connection_manager_wrapper_fn: F,
    ) -> Result<Self, AuthError>
    where
        S: Into<String>,
        F: Fn(ConnectionManager) -> ConnectionManager + Send + 'static,
        N: FnMut() + Send + 'static,
    {
        let locator = locator.into();
        let password = password.into();

        Self::login_impl(
            move |el_h, core_tx, net_tx| {
                AuthClient::login_with_hook(
                    &locator,
                    &password,
                    el_h,
                    core_tx,
                    net_tx,
                    connection_manager_wrapper_fn,
                )
            },
            disconnect_notifier,
        )
    }
}

impl Drop for Authenticator {
    fn drop(&mut self) {
        debug!("Authenticator is now being dropped.");

        let core_tx = unwrap!(self.core_tx.lock());
        let msg = CoreMsg::build_terminator();

        if let Err(e) = core_tx.unbounded_send(msg) {
            info!("Unexpected error in drop: {:?}", e);
        }
    }
}
