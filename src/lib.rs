// Copyright 2019 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

mod lib_helpers;
mod scl_mock;

pub use lib_helpers::vec_to_hex;
use lib_helpers::{
    decode_ipc_msg, encode_ipc_msg, pk_from_hex, pk_to_hex, sk_from_hex, xorname_to_xorurl,
    xorurl_to_xorname, KeyPair,
};
use log::{debug, info, warn};
use reqwest::get as httpget;
#[cfg(feature = "fake-auth")]
use safe_app::test_utils::create_app;
use safe_app::App;
use safe_core::ipc::{AppExchangeInfo, AuthReq, IpcReq};
use scl_mock::MockSCL;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use threshold_crypto::SecretKey;
use unwrap::unwrap;
use uuid::Uuid;

// Type tag used for the Wallet container
static WALLET_TYPE_TAG: u64 = 10_000;

static WALLET_DEFAULT: &str = "_default";

// Default URL where to send a GET request to the authenticator webservice for authorising a SAFE app
const SAFE_AUTH_WEBSERVICE_BASE_URL: &str = "http://localhost:41805/authorise/";

// The XOR-URL type (in the future in can be a struct with different functions)
pub type XorUrl = String;

// We expose a BLS key pair as two hex encoded strings
// TODO: consider supporting other encodings like base32 or just expose Vec<u8>
#[derive(Clone)]
pub struct BlsKeyPair {
    pub pk: String,
    pub sk: String,
}

// Struct which is serialised and stored in Wallet MD for linking to a spendable balance (Key)
#[derive(Serialize, Deserialize, Debug)]
struct WalletSpendableBalance {
    xorurl: XorUrl,
    sk: String,
}

pub struct Safe {
    safe_app: Option<App>,
    safe_app_mock: MockSCL,
    xorurl_base: String,
}

impl Safe {
    pub fn new(xorurl_base: String) -> Self {
        Self {
            safe_app: None,
            safe_app_mock: MockSCL::new(), // TODO: this will need to be replaced by auth process
            xorurl_base,
        }
    }

    // Generate an authorisation request string and send it to a SAFE Authenticator.
    // Ir returns the credentials necessary to connect to the network, encoded in a single string.
    pub fn auth_app(
        &mut self,
        app_id: &str,
        app_name: &str,
        app_vendor: &str,
    ) -> Result<String, String> {
        info!("Sending authorisation request to SAFE Authenticator...");

        let ipc_req = IpcReq::Auth(AuthReq {
            app: AppExchangeInfo {
                id: app_id.to_string(),
                scope: None,
                name: app_name.to_string(),
                vendor: app_vendor.to_string(),
            },
            app_container: false,
            // TODO: allow list of required containers permissions to be passed in as param
            containers: HashMap::new(),
        });

        match encode_ipc_msg(ipc_req) {
            Ok(auth_req_str) => {
                debug!(
                    "Authorisation request generated successfully: {}",
                    auth_req_str
                );

                let authenticator_webservice_url =
                    SAFE_AUTH_WEBSERVICE_BASE_URL.to_string() + &auth_req_str;
                let mut res = httpget(&authenticator_webservice_url).unwrap();
                let mut auth_res = String::new();
                res.read_to_string(&mut auth_res).unwrap();
                info!("SAFE authorisation response received!");

                // Check if the app has been authorised
                match decode_ipc_msg(&auth_res) {
                    Ok(_) => {
                        info!("Application was authorisaed");
                        Ok(auth_res)
                    }
                    Err(e) => {
                        info!("Application was not authorised");
                        Err(e)
                    }
                }
            }
            Err(e) => Err(format!(
                "Failed encoding the authorisation request: {:?}",
                e
            )),
        }
    }

    #[cfg(feature = "fake-auth")]
    pub fn connect(&mut self, _app_id: &str, _auth_credentials: &str) -> Result<(), String> {
        warn!("Using fake authorisation for testing...");
        self.safe_app = Some(create_app());
        Ok(())
    }

    // Connect to the SAFE Network using the provided app id and auth credentials
    #[cfg(not(feature = "fake-auth"))]
    pub fn connect(&mut self, app_id: &str, auth_credentials: &str) -> Result<(), String> {
        debug!("Connecting to SAFE Network...");

        let disconnect_cb = || {
            warn!("Connection with the SAFE Network was lost");
        };

        match decode_ipc_msg(auth_credentials) {
            Ok(auth_granted) => {
                match App::registered(app_id.to_string(), auth_granted, disconnect_cb) {
                    Ok(app) => {
                        self.safe_app = Some(app);
                        debug!("Successfully connected to the Network!!!");
                        Ok(())
                    }
                    Err(e) => Err(format!("Failed to connect to the SAFE Network: {:?}", e)),
                }
            }
            Err(e) => Err(e),
        }
    }

    // Create a Key on the network and return its XOR-URL
    pub fn keys_create(
        &mut self,
        from: BlsKeyPair,
        preload_amount: Option<String>,
        pk: Option<String>,
    ) -> (XorUrl, Option<BlsKeyPair>) {
        let from_key_pair = KeyPair::from_hex_keys(&from.pk, &from.sk);

        let create_key = |pk| match preload_amount {
            Some(amount) => self.safe_app_mock.create_balance(
                &from_key_pair.pk,
                &from_key_pair.sk,
                &pk,
                &amount,
            ),
            None => {
                self.safe_app_mock
                    .create_balance(&from_key_pair.pk, &from_key_pair.sk, &pk, "0")
            }
        };

        let (xorname, key_pair) = match pk {
            Some(pk_str) => {
                let pk = pk_from_hex(&pk_str);
                (create_key(pk), None)
            }
            None => {
                let key_pair = KeyPair::random();
                let (pk, sk) = key_pair.to_hex_key_pair();
                (create_key(key_pair.pk), Some(BlsKeyPair { pk, sk }))
            }
        };

        let xorurl = xorname_to_xorurl(&xorname, &self.xorurl_base);
        (xorurl, key_pair)
    }

    // Create a Key on the network, allocates testcoins onto it, and return the Key's XOR-URL
    // This is avilable only when testing with mock-network
    // #[cfg(feature = "mock-network")]
    pub fn keys_create_preload_test_coins(
        &mut self,
        preload_amount: String,
        pk: Option<String>,
    ) -> (XorUrl, Option<BlsKeyPair>) {
        let (xorname, key_pair) = match pk {
            Some(pk_str) => {
                let pk = pk_from_hex(&pk_str);
                let xorhash = self.safe_app_mock.allocate_test_coins(&pk, &preload_amount);
                (xorhash, None)
            }
            None => {
                let key_pair = KeyPair::random();
                let xorhash = self
                    .safe_app_mock
                    .allocate_test_coins(&key_pair.pk, &preload_amount);
                let (pk, sk) = key_pair.to_hex_key_pair();
                (xorhash, Some(BlsKeyPair { pk, sk }))
            }
        };

        let xorurl = xorname_to_xorurl(&xorname, &self.xorurl_base);
        (xorurl, key_pair)
    }

    // Check Key's balance from the network from a given PublicKey
    pub fn keys_balance_from_pk(&self, key_pair: &BlsKeyPair) -> String {
        let pair = KeyPair::from_hex_keys(&key_pair.pk, &key_pair.sk);
        self.safe_app_mock.get_balance_from_pk(&pair.pk, &pair.sk)
    }

    // Check Key's balance from the network from a given XOR-URL
    pub fn keys_balance_from_xorurl(&self, xorurl: &str, sk: &str) -> String {
        let secret_key: SecretKey = sk_from_hex(sk);
        let xorname = xorurl_to_xorname(xorurl);
        self.safe_app_mock
            .get_balance_from_xorname(&xorname, &secret_key)
    }

    // Fetch Key's pk from the network from a given XOR-URL
    pub fn keys_fetch_pk(&self, xorurl: &str) -> String {
        let xorname = xorurl_to_xorname(xorurl);
        let public_key = self.safe_app_mock.keys_fetch_pk(&xorname);
        pk_to_hex(&public_key)
    }

    // Create an empty Wallet and return its XOR-URL
    pub fn wallet_create(&mut self) -> XorUrl {
        let xorname = self.safe_app_mock.mutable_data_put(None, None, None, false);
        xorname_to_xorurl(&xorname, &self.xorurl_base)
    }

    // Add a Key to a Wallet to make it spendable
    pub fn wallet_insert(
        &mut self,
        wallet_xorurl: &str,
        name: &str,
        default: bool,
        key_pair: &BlsKeyPair,
        key_xorurl: &str,
    ) {
        let value = WalletSpendableBalance {
            xorurl: key_xorurl.to_string(),
            sk: key_pair.sk.clone(),
        };
        let serialised_value = unwrap!(serde_json::to_string(&value));
        // FIXME: it should return error if the name already exists
        let k = name.to_string().into_bytes();
        let wallet_xorname = xorurl_to_xorname(&wallet_xorurl);
        self.safe_app_mock.mutable_data_insert(
            &wallet_xorname,
            WALLET_TYPE_TAG,
            &k,
            &serialised_value.into_bytes(),
        );

        if default {
            // add the _default key
            self.safe_app_mock.mutable_data_insert(
                &wallet_xorname,
                WALLET_TYPE_TAG,
                &WALLET_DEFAULT.to_string().into_bytes(),
                &k,
            );
        }
    }

    // Check the total balance of a Wallet found at a given XOR-URL
    pub fn wallet_balance(&mut self, xorurl: &str, _sk: &str) -> String {
        let mut total_balance: f64 = 0.0;
        let wallet_xorname = xorurl_to_xorname(&xorurl);
        let spendable_balances = self
            .safe_app_mock
            .mutable_data_get_entries(&wallet_xorname, WALLET_TYPE_TAG);

        // Iterate through the Keys and query the balance for each
        spendable_balances.iter().for_each(|(name, balance)| {
            let thename = String::from_utf8_lossy(name).to_string();
            // ignore the _default Wallet MDkey
            if thename != WALLET_DEFAULT {
                let current_balance = String::from_utf8_lossy(balance).to_string();
                let spendable_balance: WalletSpendableBalance =
                    unwrap!(serde_json::from_str(&current_balance));

                let current_balance =
                    self.keys_balance_from_xorurl(&spendable_balance.xorurl, &spendable_balance.sk);
                total_balance += unwrap!(current_balance.parse::<f64>());
            }
        });
        total_balance.to_string()
    }

    fn wallet_get_default_balance(
        &mut self,
        wallet_xorurl: &str,
    ) -> Result<WalletSpendableBalance, String> {
        let xorname = xorurl_to_xorname(&wallet_xorurl);
        let mut default_key: String;

        if let Some(default) = unwrap!(self.safe_app_mock.mutable_data_get_key(
            WALLET_DEFAULT,
            &xorname,
            WALLET_TYPE_TAG
        )) {
            default_key = String::from_utf8_lossy(&default).to_string();

            info!(
                "The default WalletBalance {:?} is named \"{:?}\"",
                &wallet_xorurl, &default_key
            );
        } else {
            return Err(format!(
                "No default balance found at Wallet {:?}",
                &wallet_xorurl
            ));
        }

        let the_balance: WalletSpendableBalance =
            {
                let default_balance_vec = unwrap!(unwrap!(self
                    .safe_app_mock
                    .mutable_data_get_key(&default_key, &xorname, WALLET_TYPE_TAG)));

                let default_balance = String::from_utf8_lossy(&default_balance_vec).to_string();
                let spendable_balance: WalletSpendableBalance =
                    unwrap!(serde_json::from_str(&default_balance));
                spendable_balance
            };

        Ok(the_balance)
    }

    /// # Transfer safecoins from one Wallet to another
    ///
    /// Using established Wallet and SpendableBalances you can send safecoins between Wallets.
    ///
    /// ## Example
    /// ```
    /// # use safe_cli::Safe;
    /// # use unwrap::unwrap;
    /// let mut safe = Safe::new("base32".to_string());
    /// let sk = String::from("391987fd429b4718a59b165b5799eaae2e56c697eb94670de8886f8fb7387058");
    /// let wallet_xorurl = safe.wallet_create();
    /// let wallet_xorurl2 = safe.wallet_create();
    /// let (key1_xorurl, key_pair1) = safe.keys_create_preload_test_coins("14".to_string(), None);
    /// let (key2_xorurl, key_pair2) = safe.keys_create_preload_test_coins("1".to_string(), None);
    /// safe.wallet_insert(
    ///     &wallet_xorurl,
    ///     "frombalance",
    ///     true,
    ///     &key_pair1.unwrap(),
    ///     &key1_xorurl,
    /// );
    /// let current_balance = safe.wallet_balance(&wallet_xorurl, &sk);
    /// assert_eq!("14", current_balance);
    ///
    /// safe.wallet_insert(
    ///     &wallet_xorurl2,
    ///     "tobalance",
    ///     true,
    ///     &key_pair2.unwrap(),
    ///     &key2_xorurl,
    /// );
    ///
    ///
    /// safe.wallet_transfer( "10", Some(wallet_xorurl), &wallet_xorurl2, );
    /// let from_balance = safe.keys_balance_from_xorurl(&key1_xorurl, &sk );
    /// assert_eq!("4.", from_balance);
    /// let to_balance = safe.keys_balance_from_xorurl(&key2_xorurl, &sk );
    /// assert_eq!("11.", to_balance);
    /// ```
    pub fn wallet_transfer(
        &mut self,
        amount: &str,
        from: Option<XorUrl>,
        to: &str,
    ) -> Result<Uuid, String> {
        // from is not optional until we know default account container / Wallet location ("root")
        // if no FROM for now, ERR
        // FROM needs to be from default

        // TODO: Grab "from" stdin

        // TODO, check if to/from are WalletContainers or PKs (via safe:)
        let from_wallet_xorurl =
            match from {
                Some(wallet_xorurl) => wallet_xorurl,
                _ => return Err(
                    "A \"<from>\" wallet is required until default wallets have been configured."
                        .to_string(),
                ),
            };

        let from_wallet_balance = unwrap!(self.wallet_get_default_balance(&from_wallet_xorurl));
        let to_wallet_balance = unwrap!(self.wallet_get_default_balance(&to));

        let from_pk = self
            .safe_app_mock
            .keys_fetch_pk(&xorurl_to_xorname(&from_wallet_balance.xorurl));

        let to_pk = self
            .safe_app_mock
            .keys_fetch_pk(&xorurl_to_xorname(&to_wallet_balance.xorurl));

        let from_sk = sk_from_hex(&from_wallet_balance.sk);
        let tx_id = Uuid::new_v4();

        self.safe_app_mock
            .safecoin_transfer(&from_pk, &from_sk, &to_pk, &tx_id, amount)
    }
}

// Unit Tests

#[test]
fn test_keys_create_preload_test_coins() {
    let mut safe = Safe::new("base32".to_string());
    let (xorurl, key_pair) = safe.keys_create_preload_test_coins("12.23".to_string(), None);
    assert!(xorurl.starts_with("safe://"));
    match key_pair {
        None => panic!("Key pair was not generated as it was expected"),
        Some(_) => assert!(true),
    };
}

#[test]
fn test_keys_create_preload_test_coins_pk() {
    let mut safe = Safe::new("base32".to_string());
    let pk = String::from("a252e6741b524ad70cf340f32d219c60a3f1a38aaec0d0dbfd24ea9ae7390e44ebdc93e7575711e65379eb0f4de083a8");
    let (xorurl, key_pair) = safe.keys_create_preload_test_coins("1.1".to_string(), Some(pk));
    assert!(xorurl.starts_with("safe://"));
    match key_pair {
        None => assert!(true),
        Some(kp) => panic!("Unexpected key pair generated: {:?} {:?}", kp.pk, kp.sk),
    };
}

#[test]
fn test_keys_create() {
    let mut safe = Safe::new("base32".to_string());
    let (_, from_key_pair) = safe.keys_create_preload_test_coins("23.23".to_string(), None);

    let (xorurl, key_pair) = safe.keys_create(unwrap!(from_key_pair), None, None);
    assert!(xorurl.starts_with("safe://"));
    match key_pair {
        None => panic!("Key pair was not generated as it was expected"),
        Some(_) => assert!(true),
    };
}

#[test]
fn test_keys_create_preload() {
    let mut safe = Safe::new("base32".to_string());
    let (_, from_key_pair) = safe.keys_create_preload_test_coins("543.2312".to_string(), None);

    let preload_amount = "1.8";
    let (xorurl, key_pair) = safe.keys_create(
        unwrap!(from_key_pair),
        Some(preload_amount.to_string()),
        None,
    );
    assert!(xorurl.starts_with("safe://"));
    match key_pair {
        None => panic!("Key pair was not generated as it was expected"),
        Some(kp) => {
            let balance = safe.keys_balance_from_pk(&BlsKeyPair {
                pk: kp.pk,
                sk: kp.sk,
            });
            assert_eq!(balance, preload_amount);
        }
    };
}

#[test]
fn test_keys_create_pk() {
    let mut safe = Safe::new("base32".to_string());
    let (_, from_key_pair) = safe.keys_create_preload_test_coins("1.1".to_string(), None);
    let pk = String::from("a252e6741b524ad70cf340f32d219c60a3f1a38aaec0d0dbfd24ea9ae7390e44ebdc93e7575711e65379eb0f4de083a8");
    let (xorurl, key_pair) = safe.keys_create(unwrap!(from_key_pair), None, Some(pk));
    assert!(xorurl.starts_with("safe://"));
    match key_pair {
        None => assert!(true),
        Some(kp) => panic!("Unexpected key pair generated: {:?} {:?}", kp.pk, kp.sk),
    };
}

#[test]
fn test_keys_test_coins_balance_pk() {
    use unwrap::unwrap;
    let mut safe = Safe::new("base32".to_string());
    let preload_amount = "1.1542";
    let (_, key_pair) = safe.keys_create_preload_test_coins(preload_amount.to_string(), None);
    let current_balance = safe.keys_balance_from_pk(&unwrap!(key_pair));
    assert_eq!(preload_amount, current_balance);
}

#[test]
fn test_keys_test_coins_balance_xorurl() {
    use unwrap::unwrap;
    let mut safe = Safe::new("base32".to_string());
    let preload_amount = "0.243";
    let (xorurl, key_pair) = safe.keys_create_preload_test_coins(preload_amount.to_string(), None);
    let current_balance = safe.keys_balance_from_xorurl(&xorurl, &unwrap!(key_pair).sk);
    assert_eq!(preload_amount, current_balance);
}

#[test]
fn test_keys_balance_pk() {
    use unwrap::unwrap;
    let mut safe = Safe::new("base32".to_string());
    let preload_amount = "1743.234";
    let (_, from_key_pair) = safe.keys_create_preload_test_coins(preload_amount.to_string(), None);
    let from_key_pair_unwrapped = unwrap!(from_key_pair);

    let amount = "1740";
    let (_, to_key_pair) = safe.keys_create(
        from_key_pair_unwrapped.clone(),
        Some(amount.to_string()),
        None,
    );

    let from_current_balance = safe.keys_balance_from_pk(&from_key_pair_unwrapped);
    assert_eq!("3.234" /*== 1743.234 - 1740*/, from_current_balance);

    let to_current_balance = safe.keys_balance_from_pk(&unwrap!(to_key_pair));
    assert_eq!(amount, to_current_balance);
}

#[test]
fn test_keys_balance_xorname() {
    use unwrap::unwrap;
    let mut safe = Safe::new("base32".to_string());
    let preload_amount = "435.34";
    let (from_xorname, from_key_pair) =
        safe.keys_create_preload_test_coins(preload_amount.to_string(), None);
    let from_key_pair_unwrapped = unwrap!(from_key_pair);

    let amount = "35.3";
    let (to_xorname, to_key_pair) = safe.keys_create(
        from_key_pair_unwrapped.clone(),
        Some(amount.to_string()),
        None,
    );

    let from_current_balance =
        safe.keys_balance_from_xorurl(&from_xorname, &from_key_pair_unwrapped.sk);
    assert_eq!("400.04" /*== 435.34 - 35.3*/, from_current_balance);

    let to_current_balance = safe.keys_balance_from_xorurl(&to_xorname, &unwrap!(to_key_pair).sk);
    assert_eq!(amount, to_current_balance);
}

#[test]
fn test_keys_fetch_pk_test_coins() {
    use unwrap::unwrap;
    let mut safe = Safe::new("base32".to_string());
    let (xorurl, key_pair) = safe.keys_create_preload_test_coins("23.22".to_string(), None);
    let key_pair_unwrapped = unwrap!(key_pair);
    let pk = safe.keys_fetch_pk(&xorurl);
    assert_eq!(pk, key_pair_unwrapped.pk);
}

#[test]
fn test_keys_fetch_pk() {
    use unwrap::unwrap;
    let mut safe = Safe::new("base32".to_string());
    let (_, from_key_pair) = safe.keys_create_preload_test_coins("0.56".to_string(), None);

    let (xorurl, key_pair) = safe.keys_create(unwrap!(from_key_pair), None, None);
    let key_pair_unwrapped = unwrap!(key_pair);
    let pk = safe.keys_fetch_pk(&xorurl);
    assert_eq!(pk, key_pair_unwrapped.pk);
}

#[test]
fn test_wallet_create() {
    let mut safe = Safe::new("base32".to_string());
    let xorurl = safe.wallet_create();
    assert!(xorurl.starts_with("safe://"));
}

#[test]
fn test_wallet_insert_and_balance() {
    let mut safe = Safe::new("base32".to_string());
    let sk = String::from("391987fd429b4718a59b165b5799eaae2e56c697eb94670de8886f8fb7387058");
    let wallet_xorurl = safe.wallet_create();
    let (key1_xorurl, key_pair1) = safe.keys_create_preload_test_coins("12.23".to_string(), None);
    let (key2_xorurl, key_pair2) = safe.keys_create_preload_test_coins("1.53".to_string(), None);
    safe.wallet_insert(
        &wallet_xorurl,
        "myfirstbalance",
        true,
        &unwrap!(key_pair1),
        &key1_xorurl,
    );
    let current_balance = safe.wallet_balance(&wallet_xorurl, &sk);
    assert_eq!("12.23", current_balance);

    safe.wallet_insert(
        &wallet_xorurl,
        "mysecondbalance",
        false,
        &unwrap!(key_pair2),
        &key2_xorurl,
    );

    let current_balance = safe.wallet_balance(&wallet_xorurl, &sk);
    assert_eq!("13.76" /*== 12.23 + 1.53*/, current_balance);
}
